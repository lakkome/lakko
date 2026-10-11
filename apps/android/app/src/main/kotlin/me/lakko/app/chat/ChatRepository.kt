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
import me.lakko.app.calls.toRow
import me.lakko.app.calls.withSender
import me.lakko.app.profile.toFfi
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
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsReachAuth
import me.lakko.app.ds.PublishResult
import me.lakko.app.ds.DsSignerRegistry
import me.lakko.app.ds.dropsPendingSeed
import me.lakko.app.ds.DsSignerSource
import me.lakko.app.ds.DsWake
import me.lakko.app.ds.DsWakeKind
import me.lakko.app.ds.FfiDsJournalSigner
import me.lakko.app.ds.dsGroupKeyHex
import me.lakko.app.ds.dsMailboxKey
import me.lakko.app.reach.toKind
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.GroupKind
import uniffi.lakko_ffi_uniffi.GroupMember
import uniffi.lakko_ffi_uniffi.OutgoingOwner
import uniffi.lakko_ffi_uniffi.ProposalRecord
import uniffi.lakko_ffi_uniffi.Role
import uniffi.lakko_ffi_uniffi.RoleTable
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.loadGroup
import uniffi.lakko_ffi_uniffi.membershipRefusalRetryMaySucceed

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
internal const val POLL_ALL_CONCURRENCY: Int = 3

/** Characters of a group's DS pseudonym that identify it in a log line. Enough
 *  to follow one conversation through a run, never enough to name it. */
private const val GROUP_LOG_PREFIX_CHARS: Int = 8

/**
 * The call-routing breadcrumb, guarded the way [me.lakko.app.calls.OneToOneCallHost]
 * guards its own: `android.util.Log` is not mocked in the host test suite, so an
 * unguarded call turns every test that crosses this line into a failure about
 * logging. This line sits on a path four existing tests drive, which is how that
 * was found.
 */
private fun logCallRoute(message: String) {
    try {
        android.util.Log.i("AppCallRoute", message)
    } catch (_: Throwable) {
        // No Android logger (host unit tests): nothing to report to.
    }
}

/** Seconds-to-millis, for the one place a long-poll budget has to be honoured
 *  without an actual long poll behind it (ADR-0045 UX §6.2.8 item 1 — every
 *  group on this device has ended, so there is nothing to hold open). */
private const val MILLIS_PER_SECOND: Long = 1_000L

/** ADR-0050 section 2.8 — how long [ChatRepository.admitDevice] waits for the
 *  new device's KeyPackage to appear in the directory, and how often it looks.
 *  The fresh device publishes it after it has landed a session. */
private const val ADMIT_DEVICE_WAIT_MS: Long = 180_000L
private const val ADMIT_DEVICE_POLL_MS: Long = 3_000L
/** ADR-0066 - how long `admitDevice` waits for a readable DeviceGroup roster. */
private const val OWN_ROSTER_WAITS: Int = 10
private const val OWN_ROSTER_WAIT_MS: Long = 500L

/** ADR-0053 — at most one knock-queue read per group in this window. */
private const val KNOCK_POLL_MIN_INTERVAL_MS: Long = 10_000L

/** ADR-0053 — the [DsSignerRegistry] owner tag of the journals this repository
 *  installs, so a session wipe drops exactly those. */
private const val DS_SIGNER_OWNER: String = "chat"

/** ADR-0057 — how soon an INCOMPLETE appointment reconcile (a member whose
 *  identity bundle did not resolve) may run again from the poll tail. Chosen,
 *  not measured: long enough that a quiet chat does not repeat its discovery
 *  lookups on every long poll, short enough that a newcomer gets its seed in
 *  the same session. */
private const val APPOINTMENT_RETRY_INTERVAL_MS: Long = 60_000L

/** ADR-0057 second security pass, finding 4 — an owned group without the
 *  "root held" marker probes at most this often. Chosen, not measured. */
private const val ROOT_PROBE_RETRY_MS: Long = 60_000L

/** A takeover whose root statement failed is retried no sooner than this.
 *  Chosen, not measured. */
private const val TAKEOVER_RETRY_MS: Long = 60_000L

private const val HTTP_CONFLICT_STATUS: Int = 409

/** How many immediate re-polls a repair check may take to reach the journal
 *  head (each poll applies one page). */
private const val JOURNAL_REPAIR_MAX_POLLS: Int = 20

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
 *   * [forgetDevice] is called by the Forget device / Delete
 *     account orchestration in `MainActivity`, before
 *     `AuthRepository.forgetDevice` runs the one on-device wipe
 *     ([me.lakko.app.account.AccountLocalWipe]) that deletes the
 *     snapshot envelope and every other chat store; the only way
 *     back into the conversation history is from other devices'
 *     epochs forward (via MLS commits) once they catch up to this
 *     device's absence.
 *
 * 1.15f4b adds the Compose ChatScreen + ChatViewModel on top of
 * this; 1.15f4c wires `ContactsRepository.acceptContactRequest`
 * to call [startGroupWith] so an Accept turns into a real chat.
 */
public class ChatRepository(
    private val applicationContext: Context,
    /** Built LAZILY (cold start): each Ktor client costs an OkHttp engine + a
     *  thread pool at construction, and the repository is constructed on the
     *  main thread in `MainActivity.onCreate`, before the first frame. Nothing
     *  needs a client until the session's first network call. */
    chatClientFactory: () -> ChatClient = { ChatClient() },
    dsClientFactory: () -> DsClient = { DsClient() },
    discoveryFactory: () -> DiscoveryClient = { DiscoveryClient() },
    private val snapshotStore: IdentitySnapshotStore = IdentitySnapshotStore,
    private val knownGroupsStore: KnownGroupsStore = KnownGroupsStore(applicationContext),
    /** ADR-0042 — plaintext `push_gid -> (account, group)` reverse index the
     *  FCM silent-push decrypt reads (Sprint 4). Populated on every secret
     *  persist; the sensitive secret lives encrypted in
     *  [PushRoutingSecretStore]. */
    private val pushRouteStore: PushRouteStore = PushRouteStore(applicationContext),
    /** Builds the short-lived, bearer-authenticated discovery client every
     *  `require_session` route goes through: the KeyPackage upload and
     *  delete, and the group-invite upload and revoke. Separate from
     *  [discovery] so the session token is never attached to that shared
     *  client's anonymous requests. */
    private val keyPackageDiscoveryFactory: () -> DiscoveryClient = { DiscoveryClient() },
    /** ADR-0024 — durable per-group read watermarks, so the author-side
     *  "Read" state survives a relaunch (the poll resumes past receipts it
     *  already applied). Seeded into each group record at bootstrap / join;
     *  written off the MLS locks after a batch that carried a receipt. */
    private val readWatermarkStore: ReadWatermarkStore =
        SharedPreferencesReadWatermarkStore(applicationContext),
    /** Durable per-group group-call records, so an ongoing call survives a
     *  relaunch (the poll resumes past its announce). Seeded at bootstrap;
     *  written off the MLS locks whenever a record changes. */
    private val groupCallStore: me.lakko.app.calls.GroupCallStore =
        me.lakko.app.calls.SharedPreferencesGroupCallStore(applicationContext),
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

    private val chatClientLazy = lazy(chatClientFactory)
    private val dsClientLazy = lazy(dsClientFactory)
    private val discoveryLazy = lazy(discoveryFactory)
    private val chatClient: ChatClient by chatClientLazy
    private val dsClient: DsClient by dsClientLazy
    private val discovery: DiscoveryClient by discoveryLazy

    /**
     * The canonical [Identity] for the current session. `null`
     * until [bootstrap] populates it. Holding the reference in
     * an [AtomicReference] means snapshot writes don't need to
     * lock the whole repository — the [Identity] handle is
     * thread-safe at the UniFFI level (Rust-side `Arc`).
     */
    /** Process-lifetime scope for fire-and-forget push-routing work
     *  (ADR-0042 subscribe / unsubscribe, rotation). `SupervisorJob` so
     *  one failed leg doesn't tear down siblings; `Dispatchers.IO`
     *  because the legs are network-heavy. The group WAKE no longer runs
     *  here: since ADR-0054 it rides the signed DS publish. */
    private val pushScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** ADR-0057 — the owner's journal claims launched on open, awaited by a
     *  sweep before it registers an epoch. */
    private val ownerClaimGate = OwnerClaimGate()

    /** ADR-0045 x ADR-0057 — the per-group hold after a refused sweep
     *  registration. */
    private val sweepBackoff = SweepRegistrationBackoffTracker()

    /**
     * `docs/ux/calls.md` section 6 — process-lifetime scope for writing a
     * finished call's transcript row to disk.
     *
     * Its own scope, and on [Dispatchers.IO], because the call host runs on the
     * main dispatcher: a call teardown must not block the frame on a Keystore
     * unwrap and a whole-transcript write, and it must not be cancelled by the
     * screen the call happened to end on going away.
     */
    private val callRowScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val canonicalIdentity = AtomicReference<Identity?>(null)

    /**
     * ADR-0050 — the ACCOUNT identity (phrase-seeded), kept beside the DEVICE
     * identity [canonicalIdentity] for what is about the person rather than a
     * leaf: invite capabilities and their revocation. Every group, every
     * KeyPackage and the MLS snapshot use [canonicalIdentity], which since
     * ADR-0050 is this device's bound leaf identity.
     */
    private val accountIdentity = AtomicReference<Identity?>(null)

    /** ADR-0066 part 3 - the account identity the directory serves, when it
     *  is NOT the conversation identity ([accountIdentity]): set after a
     *  rotation of the account keys, `null` otherwise. Used only for the
     *  mailbox of the published key, the reach keys and the journal roots
     *  ([rootAccountIdentity]). */
    private val publishedAccountIdentity = AtomicReference<Identity?>(null)

    /**
     * ADR-0066 section 3.5, "DS" - the account identity the ROOT of an owned
     * journal derives from: the identity the directory serves (the new key
     * after a rotation), else the conversation identity. What the role
     * tables, the registrar-seed envelopes and the invite issuers name stays
     * [accountIdentity] (the conversation identity); only what derives from
     * the seed OUTSIDE MLS follows the rotation.
     */
    private fun rootAccountIdentity(): Identity? = publishedAccountIdentity.get() ?: accountIdentity.get()

    /**
     * Keep [publishedAccountIdentity] in step with the session's current
     * account seed. [conversation] is the conversation identity when the
     * caller has not installed it yet.
     */
    private fun refreshPublishedAccountIdentity(currentSeed: ByteArray, conversation: Identity? = null) {
        try {
            val current = Identity.fromSeed(currentSeed)
            val held = conversation ?: accountIdentity.get()
            val same = held != null && held.signaturePublicKey().contentEquals(current.signaturePublicKey())
            if (same) {
                current.close()
                publishedAccountIdentity.getAndSet(null)?.close()
                return
            }
            val before = publishedAccountIdentity.get()
            if (before != null && before.signaturePublicKey().contentEquals(current.signaturePublicKey())) {
                current.close()
                return
            }
            publishedAccountIdentity.getAndSet(current)?.close()
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "published account identity not refreshed", cause)
        }
    }

    /**
     * Account id (base64) of the session that bootstrapped this
     * repository. Used to key the on-disk snapshot envelope.
     * Wrapped in `AtomicReference` so reads from any thread
     * pick up the value the bootstrap coroutine wrote.
     */
    private val currentAccountIdB64 = AtomicReference<String?>(null)

    /**
     * The signed-in account, or `null` before bootstrap.
     *
     * Read-only, for the per-account stores that live outside this class and
     * are keyed the same way — today the cross-device sync state, which is
     * where a conversation's mute flag is (`docs/ux/calls.md` section 2.1).
     */
    public val activeAccountIdB64: String?
        get() = currentAccountIdB64.get()

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

    /** The chat list's local unread counts; see [LocalUnreadCounter]. */
    private val localUnread = LocalUnreadCounter()

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
    private val groupLocks: GroupLocks = GroupLocks(onDrain = { groupIdHex -> notifyGroupMlsIdle(groupIdHex) })

    /** Observers of [addGroupMlsIdleListener]. */
    private val groupMlsIdleListeners =
        java.util.concurrent.CopyOnWriteArrayList<(groupIdHex: String) -> Unit>()

    /**
     * Observe every moment a group's MLS work SETTLES — its per-group lock has
     * been released with no region holding it or waiting for it. See
     * [GroupLocks] for why this is the one point that may read a group's epoch
     * on behalf of something outside the chat screen, and why it never fires
     * between two queued regions.
     *
     * Called synchronously from the releasing coroutine, on whatever thread it
     * runs on: a listener must only hand the event off, and must not take the
     * group's lock itself. Returns the unsubscribe.
     */
    public fun addGroupMlsIdleListener(listener: (groupIdHex: String) -> Unit): () -> Unit {
        groupMlsIdleListeners += listener
        return { groupMlsIdleListeners -= listener }
    }

    private fun notifyGroupMlsIdle(groupIdHex: String) {
        for (listener in groupMlsIdleListeners) {
            try {
                listener(groupIdHex)
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "group MLS idle listener failed", cause)
            }
        }
    }

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

    /**
     * The dispatcher every MLS / FFI / persistence region of this repository
     * runs on. Each `public suspend fun` hops onto it at its entry
     * (`withContext(mlsDispatcher)`), so a caller may `launch` from
     * `viewModelScope` or a Compose `LaunchedEffect` — the main dispatcher —
     * and the synchronous UniFFI calls (decrypt, commit, `snapshotStorage`,
     * the Keystore-wrapped snapshot write) never run on the main thread.
     * Before this they did: the chat screen's poll loop was dispatched by
     * `AndroidUiDispatcher`, and an ANR stack showed `main` inside
     * `Group.ownCommitterRank` / `Group.members` / `Group.pendingProposals`
     * under `pollGroup` -> `sweepIfCommitter`.
     *
     * The dispatcher changes WHERE the regions run, not their ordering: the
     * per-group lock, [providerLock], the ADR-0040 persist-before-publish
     * order and the G-37 split-poll cursor rules are all coroutine `Mutex`es
     * and sequencing inside one coroutine, which hold on any thread. Every
     * shared structure the regions touch was already thread-safe (the
     * `synchronizedMap` of records, the `AtomicReference`s, `StateFlow`), and
     * the collectors stay on main. `Dispatchers.Default` rather than a single
     * thread because the work is CPU-bound crypto and the locks already give
     * the serialisation that matters; a network wait suspends and frees the
     * thread either way. The `MainThreadGuard` tripwires in the apply / sweep /
     * persist sections turn a region that slips back onto main into a debug
     * crash instead of jank.
     */
    private val mlsDispatcher: kotlinx.coroutines.CoroutineDispatcher = Dispatchers.Default

    /**
     * The account + bearer token the current session's KeyPackage is
     * published for. Set by [bootstrap], cleared by [clear]. Kept separately
     * from [canonicalIdentity] because the token is not MLS state.
     */
    private val keyPackageAccount = AtomicReference<KeyPackageAccount?>(null)

    /**
     * `true` when this session's bootstrap found a snapshot on disk it could
     * not read or restore (Keystore hiccup, corrupt envelope). That envelope
     * is still the only copy of every group's state, and the KeyPackage
     * republish persists the WHOLE provider, so while this is set its persist
     * refuses rather than overwrite it with a near-empty provider. Cleared
     * once any other path has persisted successfully (the old envelope is
     * gone by then anyway) and on [clear].
     */
    private val snapshotUnreadable = java.util.concurrent.atomic.AtomicBoolean(false)

    /** Process-lifetime scope for the fire-and-forget KeyPackage republish.
     *  Not tied to a screen, so leaving the chat UI never strands an upload
     *  half way. */
    private val keyPackageScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val keyPackageRepublisher: KeyPackageRepublisher<KeyPackageSession> =
        KeyPackageRepublisher(
            ports = object : KeyPackageRepublishPorts<KeyPackageSession> {
                override suspend fun <T> exclusiveProvider(block: suspend () -> T): T =
                    providerLock.withLock { block() }

                override fun currentSession(): KeyPackageSession? {
                    val identity = canonicalIdentity.get() ?: return null
                    val accountIdB64 = currentAccountIdB64.get() ?: return null
                    val account = keyPackageAccount.get() ?: return null
                    if (account.accountIdB64 != accountIdB64) return null
                    return KeyPackageSession(identity, account)
                }

                override fun createKeyPackage(session: KeyPackageSession): ByteArray =
                    session.identity.createKeyPackage()

                override fun persistSnapshotDurably(session: KeyPackageSession) {
                    // The session must still be the live one: persisting a
                    // replaced identity's provider under this account would
                    // overwrite the real snapshot.
                    check(canonicalIdentity.get() === session.identity) {
                        "session changed before the KeyPackage snapshot persist"
                    }
                    check(!snapshotUnreadable.get()) {
                        "the on-disk MLS snapshot was not restorable this session; " +
                            "refusing to overwrite it for a KeyPackage"
                    }
                    val blob = session.identity.snapshotStorage()
                    writeSnapshot(session.identity, session.account.accountIdB64, blob)
                }

                override suspend fun upload(session: KeyPackageSession, keyPackage: ByteArray) {
                    keyPackageDiscoveryFactory().use { client ->
                        client.setSessionToken(session.account.sessionToken)
                        // ADR-0050: the directory row is keyed by this device's
                        // leaf key.
                        client.uploadKeyPackage(
                            session.account.accountId,
                            keyPackage,
                            session.identity.signaturePublicKey(),
                        )
                    }
                }
            },
            log = { message, cause -> android.util.Log.w("ChatRepository", message, cause) },
        )

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
    public suspend fun bootstrap(secrets: IdentitySecrets): Unit =
        withContext(mlsDispatcher) { bootstrapOnMlsThread(secrets) }

    private suspend fun bootstrapOnMlsThread(secrets: IdentitySecrets) {
        val newAccountIdB64 = base64UrlSafe(secrets.accountId)
        // Refresh the publish credentials even on the same-account early
        // return below: a re-authentication hands us a newer token.
        keyPackageAccount.set(
            KeyPackageAccount(
                accountIdB64 = newAccountIdB64,
                accountId = secrets.accountId.copyOf(),
                sessionToken = secrets.sessionToken,
            ),
        )
        val existing = currentAccountIdB64.get()
        if (existing == newAccountIdB64) {
            // ADR-0066 part 3 - the account keys were rotated in this
            // session: the published identity (and its mailbox) is the new
            // one; the conversation identity stays the predecessor.
            refreshPublishedAccountIdentity(secrets.accountIdentitySeed)
            // ADR-0053 — a re-authentication of the same account still claims
            // both mailboxes ("after every login").
            claimOwnMailboxesBestEffort()
            // ADR-0066 section 3.5, "Reach": the reach keys and the mailbox
            // they are registered on follow the published key.
            rootAccountIdentity()?.let { bindReachBestEffort(newAccountIdB64, it) }
            return
        }
        if (existing != null) {
            // Different account — wipe in-memory state before
            // attaching the new Identity.
            wipeInMemoryState()
        }
        // ADR-0017: the AccountIdentity is seeded from the stable BIP39
        // phrase seed, NOT the password export key.
        //
        // ADR-0066 part 3, Stage A: after a rotation of the account keys the
        // conversation leaves that exist stay bound to the PREDECESSOR key
        // (no leaf is moved in place), so the account identity the chat
        // layer acts as - role tables, registrar-seed envelopes, the mailbox
        // Welcomes for those bindings arrive in - is the predecessor's. The
        // identity the directory serves is kept beside it for what derives
        // from the seed outside MLS: the mailbox of the published key, the
        // reach keys and the journal roots of owned groups (section 3.5).
        val conversationSeed = me.lakko.app.auth.conversationAccountSeedOf(
            applicationContext,
            newAccountIdB64,
            secrets.accountIdentitySeed,
        )
        val account = try {
            Identity.fromSeed(conversationSeed)
        } finally {
            conversationSeed.fill(0)
        }
        refreshPublishedAccountIdentity(secrets.accountIdentitySeed, conversation = account)
        // ADR-0050: every conversation leaf is this device's identity, bound to
        // the account key. A missing device seed falls back to a second unbound
        // instance of the account identity (the pre-ADR-0050 behaviour).
        val identity = me.lakko.app.auth.SessionIdentities.deviceIdentity(
            applicationContext,
            newAccountIdB64,
            account,
        ) ?: me.lakko.app.auth.conversationAccountSeedOf(
            applicationContext,
            newAccountIdB64,
            secrets.accountIdentitySeed,
        ).let { seed ->
            try {
                Identity.fromSeed(seed)
            } finally {
                seed.fill(0)
            }
        }
        // Cold start: the restore below is the single heaviest thing the app
        // does before its lists can render — the Keystore unwrap of the
        // snapshot, `restoreStorage` of the whole provider, one `loadGroup`
        // per known chat, the message-log parse. It used to run on the main
        // thread from `SessionRoot`'s `LaunchedEffect`, freezing the shell
        // for its whole duration; it now runs here on [mlsDispatcher], and
        // the shell renders a loading state until `bootstrapped` flips.
        MainThreadGuard.assertOffMain("ChatRepository.bootstrap")
        // ADR-0024 — read the persisted read watermarks BEFORE the provider
        // lock: a preferences read is disk I/O and the lock must not be held
        // across it (G-37). Seeded into each record once it is rebuilt below.
        val persistedWatermarks: Map<String, Map<MemberIdentity, Long>> = try {
            readWatermarkStore.loadAccount(newAccountIdB64)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "read watermarks not loaded", cause)
            emptyMap()
        }
        // Same reason, same place: the persisted group-call records.
        val persistedGroupCalls: Map<String, me.lakko.app.calls.GroupCallRecord> = try {
            me.lakko.app.calls.groupCallRecordsToSeed(
                groupCallStore.loadAccount(newAccountIdB64),
                System.currentTimeMillis(),
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "group call records not loaded", cause)
            emptyMap()
        }
        // Under the provider lock: `restoreStorage` replaces the WHOLE
        // provider and the per-group `loadGroup` / log seeding must land
        // before any poll's apply (which takes this same lock) can run
        // against a half-populated record set. Polls are additionally gated
        // on `bootstrapped`, which flips only at the end of this region.
        providerLock.withLock {
            // Restore the snapshot — best-effort. The Identity is
            // valid even if the restore fails (we just lose the
            // prior MLS state, peers re-bootstrap).
            val restoreResult = runCatching {
                val blob = snapshotStore.getSnapshot(applicationContext, newAccountIdB64)
                // ADR-0050 — a snapshot taken under another leaf identity (every
                // pre-ADR-0050 snapshot, a snapshot restored from another device's
                // backup) must not be restored: `restoreStorage` replaces the whole
                // provider with groups signed by a key this identity does not hold.
                // Start empty; the next persist overwrites it under the right key.
                val recordedLeaf = snapshotStore.getSnapshotLeafKey(applicationContext, newAccountIdB64)
                if (blob != null &&
                    (recordedLeaf == null || !recordedLeaf.contentEquals(identity.signaturePublicKey()))
                ) {
                    android.util.Log.w(
                        "ChatRepository",
                        "MLS snapshot belongs to another leaf identity; starting empty",
                    )
                    false
                } else if (blob != null) {
                    identity.restoreStorage(blob)
                    true
                } else {
                    false
                }
            }
            val restored = restoreResult.getOrDefault(false)
            if (restoreResult.isFailure) {
                android.util.Log.w(
                    "ChatRepository",
                    "MLS snapshot present but not restorable",
                    restoreResult.exceptionOrNull(),
                )
            }
            snapshotUnreadable.set(restoreResult.isFailure)

            canonicalIdentity.set(identity)
            accountIdentity.getAndSet(account)?.close()
            currentAccountIdB64.set(newAccountIdB64)
            feedRefresh.bind(newAccountIdB64)
            seedGroupCalls(persistedGroupCalls)

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
                        val record = GroupRecord(group = group, groupId = groupId)
                        persistedWatermarks[groupIdHex.lowercase()]?.let(record::seedReadWatermarks)
                        groups[groupIdHex] = record
                        // ADR-0053 — sign this journal's requests, and claim it
                        // (idempotent; launched, never awaited).
                        openJournal(groupIdHex)
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
                    // ADR-0063 section 5 — what the feed showed of this group.
                    record.seedFeedSeen(persisted.feedSeen.toSet(), persisted.feedSeenThrough)
                }
                publishThreadList()
            }

            mutableState.update { it.copy(bootstrapped = true) }
        }
        // ADR-0053 — sign reads of both own mailboxes and claim them.
        claimOwnMailboxesBestEffort()
        // ADR-0060 — bind the reach keys to this account and reconcile its
        // mailbox (the default setting for an account that never chose).
        // ADR-0066 section 3.5, "Reach": after a rotation the reach keys
        // derive from the published key, not the conversation identity's.
        bindReachBestEffort(newAccountIdB64, publishedAccountIdentity.get() ?: account)

        // Every session start — a password / passkey / recovery landing AND
        // the silent cold-start restore — publishes a KeyPackage minted from
        // THIS long-lived identity. Nothing else on Android publishes a usable
        // one, and the directory row may be dead (consumed by a Welcome, or
        // minted by a throwaway identity before this fix). Strictly after the
        // snapshot restore above: `restoreStorage` replaces the whole provider
        // and would erase private keys minted before it.
        // When the on-disk snapshot could not be restored, the republish's own
        // persist refuses (see [snapshotUnreadable]) and nothing is uploaded.
        requestKeyPackageRepublish()
    }

    /**
     * The whole MLS storage snapshot, for the automatic backup.
     *
     * `null` when no canonical identity is loaded - which is not the same as
     * an empty snapshot, and the caller must not upload one for the other.
     * Taken under [providerLock] on [mlsDispatcher], because one provider
     * serves every group and a snapshot captured mid-commit would record a
     * group torn between two epochs.
     */
    public suspend fun currentStorageSnapshot(): ByteArray? =
        withContext(mlsDispatcher) {
            providerLock.withLock {
                val identity = canonicalIdentity.get() ?: return@withLock null
                runCatching { identity.snapshotStorage() }.getOrNull()
            }
        }

    /**
     * Publish a KeyPackage this device can be added to a group with: mint it
     * from the canonical [Identity], persist the provider snapshot durably
     * under [providerLock], and only then upload it with the session token.
     * No upload when the persist fails (fail closed). Serialized, so a burst of
     * requests made before a run mints uploads once. See
     * [KeyPackageRepublisher] for the ordering and coalescing rules.
     */
    public suspend fun republishKeyPackage(): KeyPackageRepublishOutcome =
        withContext(mlsDispatcher) { keyPackageRepublisher.republish() }

    /**
     * Fire-and-forget [republishKeyPackage]. The request is numbered BEFORE
     * this returns, so a caller that just finished a Welcome join is
     * guaranteed a KeyPackage minted after that join.
     */
    public fun requestKeyPackageRepublish() {
        val ticket = keyPackageRepublisher.markRequested()
        keyPackageScope.launch {
            val outcome = keyPackageRepublisher.serve(ticket)
            if (outcome != KeyPackageRepublishOutcome.UPLOADED &&
                outcome != KeyPackageRepublishOutcome.COVERED
            ) {
                android.util.Log.w("ChatRepository", "KeyPackage republish ended: $outcome")
            }
        }
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
     *   2. `Group.create(identity, kind)` — fresh group with us as the
     *      founding member. [kind] is the caller's (ADR-0062): `ONE_TO_ONE`
     *      for a contact / invite accept, `GROUP` for the new-group flow,
     *      even while it holds two people. It is fixed in the GroupContext
     *      and never changes.
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
        kind: GroupKind,
        /** ADR-0060 §6 — what authorises the Welcome into the recipient's
         *  closed mailbox when the caller holds it for this flow only (the
         *  link grant of an `/i/` link). `null` picks from the grants this
         *  device holds for the recipient (their reply key after an accept,
         *  their contact key). */
        recipientReach: DsReachAuth? = null,
    ): String =
        withContext(mlsDispatcher) {
            startGroupWithOnMlsThread(
                recipientAccountId = recipientAccountId,
                recipientSignaturePubkey = recipientSignaturePubkey,
                kind = kind,
                recipientReach = recipientReach,
            )
        }

    private suspend fun startGroupWithOnMlsThread(
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
        kind: GroupKind,
        recipientReach: DsReachAuth?,
    ): String {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )

        // ADR-0050 — every device KeyPackage of the recipient: a person is
        // added as all of their devices, each its own leaf.
        val recipientDevices = discovery.fetchKeyPackages(recipientAccountId)
        if (recipientDevices.isNullOrEmpty()) {
            throw ChatError(
                "recipient has no KeyPackage uploaded yet",
                IllegalStateException("fetchKeyPackages returned no device"),
            )
        }
        val devices = recipientDevices + ownOtherDevices(identity)

        // G-37 — the group does not exist yet, so NO other operation can be
        // targeting it and there is no per-group lock to take until its id
        // exists. Only the shared provider needs guarding here.
        val created = providerLock.withLock {
            val group = Group.create(identity, kind)
            // ADR-0050 section 2.6 — every device of the recipient AND this
            // account's other devices in ONE commit. The founder is the only
            // member, so the commit fans out to nobody and is dropped; the ONE
            // Welcome carries every added leaf's secrets and goes, byte-identical,
            // to each leaf's own mailbox. One commit per device would strand
            // every device but the last at a Welcome epoch nobody could reach.
            val addResult = try {
                group.addMembersFromWire(identity, devices.map { it.keyPackage })
            } catch (e: FfiException) {
                group.close()
                throw ChatError("addMembersFromWire failed", e)
            }
            val welcomeEnvelope = wrapEnvelope(KIND_WELCOME, addResult.welcome)
            // ADR-0060 §6 — one Welcome write per added ACCOUNT, into its
            // account mailbox (ADR-0050 §2.7 as amended): the recipient's and,
            // when this account has other devices, this account's own.
            val accounts = welcomeAccounts(devices.map { it.keyPackage }) { kp ->
                try {
                    uniffi.lakko_ffi_uniffi.keyPackageAccountKey(kp)
                        ?: uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(kp)
                } catch (_: Throwable) {
                    null
                }
            }
            CreatedGroup(
                group = group,
                groupId = group.groupId(),
                recipientAccounts = accounts,
                welcomeEnvelope = welcomeEnvelope,
                // ADR-0053 — the founder claims the journal at the epoch the
                // invitees' Welcome lands them at.
                accessSigner = try {
                    FfiDsJournalSigner(group.dsAccessSigner(identity))
                } catch (e: Throwable) {
                    null
                },
            )
        }

        // ADR-0057 obligation 1 — the founder is the OWNER: it claims the
        // journal with its ROOT at the epoch the invitees' Welcome lands them
        // at, BEFORE the Welcome goes out, so no invitee ever reaches an
        // unclaimed journal. FAIL-CLOSED (the second security pass, finding
        // 3): a claim that does not land aborts the creation with nothing
        // published and the local group dropped — whoever claimed first would
        // otherwise be root for good.
        try {
            val signer = created.accessSigner
                ?: throw me.lakko.app.ds.DsRegistrationUnavailable("the founding access key could not be derived")
            val root = rootStatementSigner(created.groupId)
                ?: throw me.lakko.app.ds.DsRegistrationUnavailable("the journal root could not be derived")
            me.lakko.app.ds.requireClaim {
                me.lakko.app.ds.claimOrReset(
                    journalId = created.groupId,
                    epoch = signer.epoch,
                    epochKey = signer.verifyingKey,
                    statements = root,
                    refounded = false,
                    put = { make -> dsClient.putStatement(make) },
                )
            }
            markRootHeld(DsClient.pseudonymHex(created.groupId))
        } catch (e: kotlinx.coroutines.CancellationException) {
            runCatching { created.group.close() }
            throw e
        } catch (e: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0057: claim at creation did not land; not created", e)
            runCatching { created.group.close() }
            throw ChatError(dsErrorMessage(e, "Could not reach the server. Nothing was sent."), e)
        }

        // G-37 — the DS mailbox publishes run OUTSIDE the provider lock. A
        // publish failure still throws before the group is registered or
        // persisted, exactly as when one mutex covered the whole block.
        val createdHex = DsClient.pseudonymHex(created.groupId)
        // ADR-0066 3.5a rule 1 — the recipient is the PERSON: a leaf bound
        // to a proven predecessor of the key the caller named is theirs, and
        // two keys of one person are one mailbox (written once).
        val recipientCurrent = mailboxAccountKeyOf(recipientSignaturePubkey)
        val writtenMailboxes = HashSet<String>()
        for (account in created.recipientAccounts) {
            val explicit =
                if (mailboxAccountKeyOf(account).contentEquals(recipientCurrent)) recipientReach else null
            val mailbox = welcomeMailboxFor(account)
            if (!writtenMailboxes.add(DsClient.pseudonymHex(mailbox))) continue
            try {
                publishWelcomeToMailbox(mailbox, created.welcomeEnvelope, createdHex, explicit)
            } catch (e: Throwable) {
                throw ChatError(dsErrorMessage(e, "DS publishMailbox (Welcome) failed"), e)
            }
        }

        val groupIdHex = createdHex
        groupLock(groupIdHex).withLock {
            providerLock.withLock {
                groups[groupIdHex] =
                    GroupRecord(group = created.group, groupId = created.groupId)
                // Claimed above; only the signer is installed here.
                installJournalSigner(groupIdHex)
                // ADR-0057 obligation 3 — appoint the invitees (their seeds
                // are sealed into the group, which they read from the epoch
                // the Welcome lands them at). Launched; retried on open.
                reconcileAppointmentsBestEffort(groupIdHex, force = true)
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
        // ADR-0048 §2 — founding a group is joining it: publish this device's
        // profile so the invitee learns the founder's name. Launched, never
        // awaited: the publish takes the per-group lock itself.
        profileRepublish.publishNow(groupIdHex)
        return groupIdHex
    }

    /** Who the KeyPackage is published for: the snapshot key, the raw
     *  account id the directory expects, and the bearer token. */
    private class KeyPackageAccount(
        val accountIdB64: String,
        val accountId: ByteArray,
        val sessionToken: String,
    )

    /** One KeyPackage republish run's view of the session, captured under
     *  [providerLock] so mint, persist and upload all name the same account. */
    private class KeyPackageSession(
        val identity: Identity,
        val account: KeyPackageAccount,
    )

    /** G-37 — carries a freshly-created group across the provider-lock release
     *  so the Welcome mailbox publish can run outside it. */
    /**
     * ADR-0060 §6 — the account key behind each Welcome mailbox this process
     * addressed, so the write (or its restart re-publish) picks the grant for
     * that person.
     */
    private val welcomeTargetAccounts = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()

    /**
     * ADR-0066 section 3.5a rule 1 — the account key whose mailbox is
     * addressed for the person known by [accountKey] (a leaf's binding, a
     * contact row, this account's own conversation key): the key its PROVEN
     * steps lead to, else the key itself. An unproven change is never
     * followed. The identity when the reach manager cannot answer.
     */
    private fun mailboxAccountKeyOf(accountKey: ByteArray): ByteArray =
        try {
            me.lakko.app.reach.ReachManager.get(applicationContext).mailboxAccountKey(accountKey)
        } catch (_: Throwable) {
            accountKey.copyOf()
        }

    /**
     * The account mailbox a Welcome for [accountKey] goes to, remembered for
     * [publishWelcomeToMailbox]. THE one place a Welcome's mailbox is
     * derived (group creation, the direct add, the sweep's fan-out, another
     * device of this account): [accountKey] is what the added leaf's binding
     * names, which after a rotation of that account's keys is a proven
     * predecessor, so it is resolved first and the CURRENT key's mailbox is
     * addressed, with the grants held for that person (ADR-0066 3.5a).
     */
    private fun welcomeMailboxFor(accountKey: ByteArray): ByteArray {
        val current = mailboxAccountKeyOf(accountKey)
        val mailbox = uniffi.lakko_ffi_uniffi.IdentityPseudonym.fromIdentity(current).use { it.asBytes() }
        welcomeTargetAccounts[DsClient.pseudonymHex(mailbox)] = current
        return mailbox
    }

    /**
     * ADR-0060 §2 / §6 — write a Welcome into an account mailbox, signed with
     * [explicit] when the caller holds the grant for this flow, else with the
     * grants this device holds for the person
     * ([me.lakko.app.reach.ReachManager.reachAuthCandidates]: their reply key,
     * contact key, `member` key of [groupIdHex], any group / channel key, a
     * committer delegation...), each tried in turn and the next one after a
     * `401` (ADR-0050 section 2.5a item 7). This account's own mailbox and an
     * [explicit] grant are written exactly as before.
     *
     * With no grant left:
     *   * [requireGrant] (the sweep's Welcome) — nothing is written: a write
     *     with no proof to a closed mailbox is answered as stored and dropped,
     *     so it would count a lost Welcome as delivered. The person is noted
     *     ([undeliveredWelcomeGroups]) and [WelcomeUndeliverable] keeps the
     *     Welcome owed; the next poll and a new grant for them retry it.
     *   * otherwise (the direct add and group-creation paths, unchanged) — no
     *     grant at all goes unsigned, which an open mailbox takes; grants that
     *     were all refused re-throw the last refusal.
     */
    private suspend fun publishWelcomeToMailbox(
        mailbox: ByteArray,
        bytes: ByteArray,
        groupIdHex: String?,
        explicit: DsReachAuth? = null,
        requireGrant: Boolean = false,
    ): PublishResult {
        if (explicit != null) return dsClient.publishMailbox(mailbox, bytes, reach = explicit)
        val account = welcomeTargetAccounts[DsClient.pseudonymHex(mailbox)]
        if (account == null) {
            android.util.Log.i("ChatRepository", "ADR-0060: Welcome written without a reach grant")
            return dsClient.publishMailbox(mailbox, bytes, reach = null)
        }
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        if (account.contentEquals(reach.ownAccountKey() ?: ByteArray(0))) {
            // Another device of this account: its own `contact` key, as before.
            return dsClient.publishMailbox(
                mailbox,
                bytes,
                reach = reach.reachAuthFor(account, me.lakko.app.reach.WritePurpose.WELCOME, groupIdHex),
            )
        }
        val accountHex = DsClient.pseudonymHex(account)
        return try {
            writeWelcomeWithGrants(
                candidates = reach.reachAuthCandidates(account, me.lakko.app.reach.WritePurpose.WELCOME, groupIdHex),
                write = { auth ->
                    if (auth == null) {
                        android.util.Log.i("ChatRepository", "ADR-0060: Welcome written without a reach grant")
                    }
                    dsClient.publishMailbox(mailbox, bytes, reach = auth)
                },
                isRefusal = { it is me.lakko.app.ds.DsReachRefusedError },
                requireGrant = requireGrant,
                accountKeyHex = accountHex,
            )
        } catch (undeliverable: WelcomeUndeliverable) {
            if (groupIdHex != null) {
                undeliveredWelcomeGroups.getOrPut(accountHex) {
                    java.util.concurrent.ConcurrentHashMap.newKeySet()
                }.add(groupIdHex)
            }
            android.util.Log.i(
                "ChatRepository",
                "ADR-0050 2.5a: Welcome kept owed; no reach grant accepted for the added account",
            )
            throw undeliverable
        }
    }

    /**
     * ADR-0050 section 2.5a item 7 — the groups whose sweep Welcome is owed to
     * an account no grant reached yet, by account key (hex). A grant for that
     * account arriving ([me.lakko.app.reach.ReachManager.onGrantHeld]) re-runs
     * the sweep of each, which retries the owed Welcome first. In memory: after
     * a restart the poll retries every owed Welcome anyway.
     */
    private val undeliveredWelcomeGroups =
        java.util.concurrent.ConcurrentHashMap<String, MutableSet<String>>()

    /** A grant (or delegation) for [ownerKey] was kept: retry the Welcomes
     *  owed to that account. Launched, never awaited. */
    private fun retryOwedWelcomesFor(ownerKey: ByteArray) {
        val groupsOwed = undeliveredWelcomeGroups.remove(DsClient.pseudonymHex(ownerKey)) ?: return
        for (groupIdHex in groupsOwed) {
            pushScope.launch {
                try {
                    sweepIfCommitter(groupIdHex)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0050 2.5a: owed Welcome retry failed", cause)
                }
            }
        }
    }

    /**
     * The sweep Welcomes this session already wrote, by mailbox and bytes, so
     * a fan-out retried because ANOTHER target was undeliverable does not
     * write a second copy into a mailbox that took one. A copy written again
     * after a restart is inert (the joiner's KeyPackage is consumed by the
     * first), so this is in memory.
     */
    private val sweepWelcomesWritten = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** The sweep's mailbox write: [publishWelcomeToMailbox] with a grant
     *  required, once per mailbox and bytes per session. */
    private suspend fun publishSweepWelcome(mailbox: ByteArray, bytes: ByteArray, groupIdHex: String): Long {
        val key = DsClient.pseudonymHex(mailbox) + ":" + DsClient.pseudonymHex(
            java.security.MessageDigest.getInstance("SHA-256").digest(bytes),
        )
        sweepWelcomesWritten[key]?.let { return it }
        val id = publishWelcomeToMailbox(mailbox, bytes, groupIdHex, requireGrant = true).id
        sweepWelcomesWritten[key] = id
        return id
    }

    /** ADR-0060 §6 — where the delegation broadcasts run: they take the
     *  per-group lock, so they are launched, never awaited under it. */
    private val reachScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * ADR-0060 §6 — an Add this device proposed for [addedAccount]: when this
     * device is the added person's contact, sign one delegation per committer
     * candidate ([candidates], owner first, then admins by rank) and send them
     * in the group, so whichever of them sweeps the Add can deliver the
     * Welcome into the added person's closed mailbox. Nothing is sent when
     * this device holds no contact grant for them (another path must carry
     * the Welcome: their reply key, or their setting).
     */
    private fun sendCommitterDelegationsBestEffort(
        groupIdHex: String,
        addedAccount: ByteArray,
        candidates: List<ByteArray>,
    ) {
        if (candidates.isEmpty()) return
        reachScope.launch {
            try {
                val envelope = me.lakko.app.reach.ReachManager.get(applicationContext)
                    .composeDelegations(addedAccount, candidates) ?: return@launch
                val identity = canonicalIdentity.get() ?: return@launch
                val record = groups[groupIdHex] ?: return@launch
                broadcastControlToGroup(
                    identity = identity,
                    groupIdHex = groupIdHex,
                    group = record.group,
                    envelope = envelope,
                )
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0060 delegations not sent: ${cause.javaClass.simpleName}")
            }
        }
    }

    /** See [pollKnocksIfOwnerOrAdmin]. Runs under the provider lock. */
    private fun keepJoinerReplyGrantsBestEffort(record: GroupRecord, rows: List<me.lakko.app.ds.MailboxMessage>) {
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        val groupId = record.groupId
        for (row in rows) {
            val attr = try {
                uniffi.lakko_ffi_uniffi.parseGroupJoinAttribution(row.payload)
            } catch (_: Throwable) {
                continue
            }
            if (!attr.groupId.contentEquals(groupId)) continue
            if (attr.replyGrant.`class` != uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY) continue
            val account = try {
                record.group.pendingExternalJoinAccountKey(attr.joinerSignatureKey)
            } catch (_: Throwable) {
                null
            } ?: attr.joinerSignatureKey
            reach.keepHeld(account, attr.replyGrant, scopeHex = "")
        }
    }

    /** ADR-0060 — bind the reach manager to the signed-in account, then
     *  publish the joined groups it keys `member` / `group` keys on. */
    private fun bindReachBestEffort(accountIdB64: String, account: Identity) {
        try {
            val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
            reach.onVisibilityChanged = { onVisibilityChanged(it) }
            // ADR-0048 section 4a / ADR-0060 section 5 — Everyone follows the
            // name, an adopted one included.
            reach.onOwnNameAdopted = { refreshEveryoneBestEffort() }
            // ADR-0050 section 2.5a item 7 — a Welcome kept owed for want of a
            // grant is retried the moment one for that person arrives.
            reach.onGrantHeld = { ownerKey -> retryOwedWelcomesFor(ownerKey) }
            reach.bind(accountIdB64, account)
            // ADR-0066 3.5a rule 3 — a re-send the key change left owed (a
            // kept device right after adopting; any device after a restart).
            if (reach.profileResendOwed()) {
                reachScope.launch {
                    try {
                        resendProfileAfterKeyChange()
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (cause: Throwable) {
                        android.util.Log.w("ChatRepository", "ADR-0066 3.5a: profile re-send failed: ${cause.javaClass.simpleName}")
                    }
                }
            }
            publishThreadList()
            // ADR-0060 §4 — the introduction record and the relayed bundle,
            // now and on the periodic re-check.
            runIntroductionsBestEffort()
            startIntroductionTicker()
            // ADR-0060 §5 — the username certificate this account's profile
            // carries, so a viewer shows the username without a lookup. A
            // first certificate republishes the profile everywhere.
            val token = keyPackageAccount.get()?.sessionToken
            if (token != null && reach.usernameCertificate() == null) {
                reachScope.launch {
                    try {
                        val cert = keyPackageDiscoveryFactory().use { client ->
                            client.setSessionToken(token)
                            client.fetchUsernameCertificate()
                        } ?: return@launch
                        if (reach.keepUsernameCertificate(cert)) publishOwnProfileToAllGroups()
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (cause: Throwable) {
                        android.util.Log.i("ChatRepository", "ADR-0060: username certificate not fetched: ${cause.javaClass.simpleName}")
                    }
                }
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0060: reach keys not bound: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * ADR-0060 §3 — the setting changed: every group's profile carries (or
     * stops carrying) the `group` grant and the username, so republish it
     * everywhere. The mailbox was already reconciled by the manager.
     */
    private suspend fun onVisibilityChanged(setting: uniffi.lakko_ffi_uniffi.FfiVisibilitySetting) {
        publishOwnProfileToAllGroups()
        visibilityListener?.invoke(setting)
    }

    /** ADR-0060 §5 / §4 — Everyone publication and introduction offers,
     *  installed by the app shell. */
    @Volatile
    public var visibilityListener: (suspend (uniffi.lakko_ffi_uniffi.FfiVisibilitySetting) -> Unit)? = null

    private class CreatedGroup(
        val group: Group,
        val groupId: ByteArray,
        /** ADR-0060 §6 — the account key of every added person. */
        val recipientAccounts: List<ByteArray>,
        val welcomeEnvelope: ByteArray,
        val accessSigner: DsJournalSigner?,
    )

    /**
     * ADR-0050 — this account's OTHER devices' KeyPackages, so a group this
     * device creates includes the person's other devices from the first epoch.
     * Best-effort: without a session or on a directory failure it is empty, and
     * the re-admission arm adds the missing devices later.
     */
    private suspend fun ownOtherDevices(identity: Identity): List<me.lakko.app.discovery.DeviceKeyPackage> {
        val account = keyPackageAccount.get() ?: return emptyList()
        val ownKey = identity.signaturePublicKey()
        return try {
            discovery.fetchKeyPackages(account.accountId)
                .orEmpty()
                .filterNot { it.deviceId.contentEquals(ownKey) }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            android.util.Log.w("ChatRepository", "own devices lookup failed; adding this device only", e)
            emptyList()
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
    public suspend fun joinGroupFromWelcome(welcomeBytes: ByteArray): String =
        withContext(mlsDispatcher) { joinGroupFromWelcomeOnMlsThread(welcomeBytes = welcomeBytes) }

    private suspend fun joinGroupFromWelcomeOnMlsThread(welcomeBytes: ByteArray): String {
        // ADR-0060 §6 — every device of the account reads the one account
        // mailbox, so a Welcome sealed to ANOTHER of its devices fails here
        // with no matching KeyPackage. That says nothing about this device's
        // published KeyPackage, so it is the one outcome that does not
        // republish.
        var forAnotherDevice = false
        try {
            return joinGroupFromWelcomeAttempt(welcomeBytes)
        } catch (e: Throwable) {
            forAnotherDevice = isWelcomeForAnotherDevice(e)
            throw e
        } finally {
            // After EVERY other attempt, success or failure. A successful join
            // consumed the published KeyPackage (OpenMLS deletes its private
            // key; the directory holds one row per account), so the row is now
            // dead. A failed join is often the symptom of a dead row — e.g.
            // one minted before this fix by a throwaway identity — and a fresh
            // one is the only way the NEXT adder can succeed. Numbered here,
            // after the join, so it is served by a KeyPackage minted after it.
            if (!forAnotherDevice) requestKeyPackageRepublish()
        }
    }

    private suspend fun joinGroupFromWelcomeAttempt(welcomeBytes: ByteArray): String {
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
            // ADR-0053 — sign this journal's requests from now on, and claim it
            // at the epoch the Welcome brought us to (idempotent).
            openJournal(groupIdHex)
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
        // ADR-0024 — a group re-joined under the same id keeps the read state
        // this device already persisted for it. Read outside the provider lock.
        seedReadWatermarksBestEffort(groupIdHex)
        publishThreadList()
        // ADR-0048 §2 — publish this device's profile into the group it just
        // joined: nobody there knows its name yet.
        profileRepublish.publishNow(groupIdHex)
        // ADR-0048 trigger 4 — and ask the group for everyone else's: the
        // growth re-sends fire on the OTHER devices, and one that is away when
        // this join lands never re-sends. Launched, never awaited: the publish
        // takes the per-group lock itself.
        profileRequestScope.launch { requestProfilesBestEffort(groupIdHex, joinedJustNow = true) }
        // ADR-0060 §4 — a new one-to-one gets the introduction record and the
        // relayed bundle.
        runIntroductionsBestEffort()
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
     * Where one-to-one call signaling goes: the app-level
     * [me.lakko.app.calls.OneToOneCallHost.deliver], installed by `MainActivity`.
     *
     * One hook for the whole repository, fed by EVERY poll of every group
     * ([routeParsedCallSignaling]). It used to be a slot each `ChatViewModel`
     * overwrote with a closure filtered to its own chat, so an offer to a chat
     * that was not open was dropped while the cursor advanced past it and the
     * call never rang. `null` (tests, before wiring) drops the body.
     *
     * Invoked inside the poll's critical section with the row's DS message id
     * and `created_at`; the host only checks its high-water mark and enqueues.
     */
    @Volatile
    internal var oneToOneCallInbound:
        ((groupIdHex: String, messageId: Long, body: uniffi.lakko_ffi_uniffi.FfiCallSignalingBody, createdAt: String) -> Unit)? =
        null

    /**
     * UX 16.4 — told the moment a one-to-one closes after the peer's key
     * reset, so a call live in that chat ends locally (its `End` frame can no
     * longer be published). Installed with [oneToOneCallInbound]; `null`
     * (tests, before wiring) does nothing.
     */
    @Volatile
    internal var oneToOneClosedSink: ((groupIdHex: String) -> Unit)? = null

    private val mutableGroupCalls: MutableStateFlow<Map<String, me.lakko.app.calls.GroupCallRecord>> =
        MutableStateFlow(emptyMap())

    /**
     * The ongoing-group-call ledger, one record per chat, keyed by group id hex.
     *
     * Held HERE rather than in a `ChatViewModel` because the app polls EVERY
     * group, not only the open chat: a group-call announce that lands for a chat
     * that is not open would otherwise be dropped while the cursor advances past
     * it, and the chat would never learn the call exists. The ledger is fed by
     * every poll of every group ([routeParsedCallSignaling]), open or not.
     *
     * Persisted per account in [groupCallStore] and seeded at bootstrap, so a
     * call started before a restart is still offered after it (the cursor is
     * already past the announce and would never deliver it again).
     */
    internal val groupCalls: StateFlow<Map<String, me.lakko.app.calls.GroupCallRecord>> =
        mutableGroupCalls.asStateFlow()

    /**
     * Fold one group-call signal into [groupIdHex]'s ledger record. Called for
     * every received body by [routeParsedCallSignaling] (with the DS row's
     * `created_at`) and by the chat screen for every body THIS device publishes
     * (with the local clock) — a device never receives its own application
     * messages back, so without the second caller its own record would never
     * count itself.
     */
    internal fun applyGroupCallSignal(
        groupIdHex: String,
        signal: me.lakko.app.calls.GroupCallSignal,
        observedAtMs: Long,
        nowMs: Long = System.currentTimeMillis(),
    ) {
        var changed = false
        mutableGroupCalls.update { calls ->
            val before = calls[groupIdHex]
            val after = me.lakko.app.calls.reduceGroupCall(before, signal, observedAtMs, nowMs)
            changed = after != before
            when {
                after == before -> calls
                after == null -> calls - groupIdHex
                else -> calls + (groupIdHex to after)
            }
        }
        if (changed) persistGroupCall(groupIdHex)
    }

    /** Guards the read-then-write in [persistGroupCall], so two writes landing
     *  out of order on the IO pool can never leave an older record on disk. */
    private val groupCallPersistLock = Any()

    /**
     * Write [groupIdHex]'s CURRENT record to [groupCallStore]. LAUNCHED onto
     * [pushScope] (`Dispatchers.IO`): callers run inside the poll's locked apply
     * region, and neither the main thread nor a per-group / provider lock may be
     * held across disk I/O (G-37). The value written is read at write time under
     * [groupCallPersistLock], so the last write always stores the latest state.
     * The account is captured now and re-checked, so a write that outlives a
     * logout / forget-device does not resurrect wiped rows.
     */
    private fun persistGroupCall(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        pushScope.launch {
            synchronized(groupCallPersistLock) {
                if (currentAccountIdB64.get() != accountIdB64) return@launch
                val row = mutableGroupCalls.value[groupIdHex]?.toRow()
                try {
                    groupCallStore.save(accountIdB64, groupIdHex, row)
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "group call record not persisted", cause)
                }
            }
        }
    }

    /** Seed the ledger from the persisted records at bootstrap. Signals already
     *  folded for a group this session win over its stored record. */
    private fun seedGroupCalls(stored: Map<String, me.lakko.app.calls.GroupCallRecord>) {
        if (stored.isEmpty()) return
        mutableGroupCalls.update { calls -> stored + calls }
    }

    /** This device's identity as the group-call ledger keys it: its own leaf
     *  signature key, which is what [resolveLeafSignatureKey] resolves this
     *  device's rows to on every other member. Per device, not per person, so
     *  another device of this account in the same call still counts as someone
     *  else. */
    internal fun ownGroupCallMemberKey(): MemberIdentity? =
        canonicalIdentity.get()?.let { identity ->
            try {
                outboxB64(identity.signaturePublicKey())
            } catch (cause: Throwable) {
                null
            }
        }

    /** This device's person key, the way [resolveMemberIdentity] resolves its
     *  rows on every other member. */
    private fun ownGroupCallPersonKey(): MemberIdentity? =
        canonicalIdentity.get()?.let { identity ->
            try {
                ownPersonKeyOf(identity)
            } catch (cause: Throwable) {
                null
            }
        }

    /**
     * Route one received call-signaling payload: parse, then
     * [routeParsedCallSignaling]. A payload that does not parse is dropped.
     *
     * Runs inside the poll's critical section, so it does only a parse and an
     * atomic state update / enqueue.
     */
    private fun routeIncomingCallSignaling(
        groupIdHex: String,
        messageId: Long,
        payload: ByteArray,
        createdAt: String,
        senderLeaf: UInt?,
        senderKey: MemberIdentity?,
        senderPersonKey: MemberIdentity?,
    ) {
        val body = try {
            uniffi.lakko_ffi_uniffi.parseCallSignaling(payload)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "call signaling: parse failed, dropped", cause)
            return
        }
        routeParsedCallSignaling(groupIdHex, messageId, body, createdAt, senderLeaf, senderKey, senderPersonKey)
    }

    /**
     * Route one parsed call-signaling body.
     *
     * Group-control bodies go to the ledger and NEVER to [oneToOneCallInbound] —
     * that hook feeds the 1x1 call host (C-11). `CallEnd` goes to both; each
     * side drops it unless it names its own call on its own group (C-12).
     */
    internal fun routeParsedCallSignaling(
        groupIdHex: String,
        messageId: Long,
        body: uniffi.lakko_ffi_uniffi.FfiCallSignalingBody,
        createdAt: String,
        senderLeaf: UInt?,
        /** The sender leaf's own signature key resolved at receipt. A Join /
         *  Leave without one changes no record. */
        senderKey: MemberIdentity? = null,
        /** The sender's person key resolved at receipt; `null` counts the
         *  device as its own person. */
        senderPersonKey: MemberIdentity? = null,
    ) {
        val route = me.lakko.app.calls.callSignalingRoute(body)
        if (route != me.lakko.app.calls.CallSignalingRoute.ONE_TO_ONE_ONLY) {
            // A group-control body may only speak for its own sender: the leaf
            // it names is an unauthenticated field, the MLS sender leaf of the
            // row is not. A mismatch is dropped (web parity).
            me.lakko.app.calls.groupCallSignalForRow(body, senderLeaf, senderKey, senderPersonKey)
                ?.let { signal ->
                    val now = System.currentTimeMillis()
                    applyGroupCallSignal(
                        groupIdHex = groupIdHex,
                        signal = signal,
                        observedAtMs = GroupRecord.parseIsoMillis(createdAt) ?: now,
                        nowMs = now,
                    )
                }
        }
        if (route != me.lakko.app.calls.CallSignalingRoute.GROUP_LEDGER_ONLY) {
            // The one place that says a call frame reached the call host, so a
            // live run can tell "never fetched" from "fetched and dropped" —
            // which is the distinction the own-echo defect turned on, and which
            // nothing in this chain reported. Kept rather than removed: the
            // whole receive path is best-effort and silent by design, and the
            // handover from the poll to the call host is the one hop where
            // losing a frame is a call that never rings.
            //
            // Identifiers only, never content: the group is an 8-character
            // prefix of its DS pseudonym (already a blinded id, and a prefix of
            // it cannot be joined back to a conversation from a log), the DS row
            // id, and the frame kind. No SDP, no candidates, no call id.
            logCallRoute(
                "call frame routed group=${groupIdHex.take(GROUP_LOG_PREFIX_CHARS)} " +
                    "row=$messageId kind=${me.lakko.app.calls.callSignalingBodyKindName(body)} " +
                    "sink=${if (oneToOneCallInbound != null) "attached" else "absent"}",
            )
            oneToOneCallInbound?.invoke(groupIdHex, messageId, body, createdAt)
        }
    }

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
    ): CallKeyMaterial? =
        withContext(mlsDispatcher) { deriveCallKeyOnMlsThread(groupIdHex = groupIdHex, callId = callId) }

    private suspend fun deriveCallKeyOnMlsThread(
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
     * G-18 / ADR-0029 — one consistent read of [groupIdHex] for the app-level
     * group-call host: membership, epoch, and the per-call seed at that epoch.
     *
     * Taken under [providerLock] alone (never the per-group lock, so it can be
     * called from a [addGroupMlsIdleListener] hand-off), and `settled` is judged
     * inside that same region: a region on the group that had not started by
     * then must take the provider lock before it can move the epoch, so it
     * cannot move it under this read; one that had started makes `settled`
     * false and reports its own drain later.
     */
    public suspend fun readSettledCallKey(
        groupIdHex: String,
        callId: ByteArray,
    ): me.lakko.app.calls.SettledGroupRead =
        withContext(mlsDispatcher) { readSettledCallKeyOnMlsThread(groupIdHex = groupIdHex, callId = callId) }

    private suspend fun readSettledCallKeyOnMlsThread(
        groupIdHex: String,
        callId: ByteArray,
    ): me.lakko.app.calls.SettledGroupRead {
        val identity = canonicalIdentity.get()
            ?: return me.lakko.app.calls.SettledGroupRead.notJoined()
        return providerLock.withLock {
            val record = groups[groupIdHex]
            if (record == null) {
                me.lakko.app.calls.SettledGroupRead.notJoined()
            } else {
                val settled = !groupLocks.isBusy(groupIdHex)
                val isActive = readIsActive { record.group.isActive() }
                val epoch = try {
                    record.group.epoch()
                } catch (_: Throwable) {
                    null
                }
                var seed: ByteArray? = null
                var leaf: Int? = null
                if (isActive != false) {
                    try {
                        seed = record.group.exportSecret(identity, "lakko/call/v1", callId, 32u)
                        leaf = record.group.ownLeafIndex().toInt()
                    } catch (cause: Throwable) {
                        android.util.Log.w("ChatRepository", "readSettledCallKey: exporter failed", cause)
                        seed = null
                        leaf = null
                    }
                }
                me.lakko.app.calls.SettledGroupRead(
                    joined = true,
                    isActive = isActive,
                    settled = settled,
                    epoch = epoch,
                    seed = seed,
                    localLeaf = leaf,
                )
            }
        }
    }

    /**
     * Is the recorded group call [callIdHex] in [groupIdHex] still live for a
     * device that is about to JOIN it? This device is not in that call yet, so
     * its own entry (a session from before a relaunch) is ignored.
     */
    internal fun isGroupCallLive(groupIdHex: String, callIdHex: String, nowMs: Long): Boolean {
        val record = groupCalls.value[groupIdHex] ?: return false
        return record.callIdHex == callIdHex &&
            me.lakko.app.calls.isGroupCallLive(record, nowMs, ignoreDeviceKey = ownGroupCallMemberKey())
    }

    /** The fresh members of the recorded group call [callIdHex] other than this
     *  device's own identity, or `null` when the record is gone or names another
     *  call. */
    internal fun groupCallOtherFreshMembers(groupIdHex: String, callIdHex: String, nowMs: Long): Set<String>? =
        me.lakko.app.calls.otherFreshGroupCallMembers(
            groupCalls.value[groupIdHex],
            callIdHex,
            ownGroupCallMemberKey(),
            nowMs,
        )

    /**
     * Compose + publish one group-call body through the locked call-signaling
     * publish path, then fold it into this device's own ledger record (it never
     * receives its own application message back). Returns `false` — and leaves
     * the ledger alone — when the publish did not happen, so the local record
     * never claims more than the group was told.
     */
    internal suspend fun publishGroupCallSignal(
        groupIdHex: String,
        body: uniffi.lakko_ffi_uniffi.FfiCallSignalingBody,
    ): Boolean =
        withContext(mlsDispatcher) { publishGroupCallSignalOnMlsThread(groupIdHex = groupIdHex, body = body) }

    private suspend fun publishGroupCallSignalOnMlsThread(
        groupIdHex: String,
        body: uniffi.lakko_ffi_uniffi.FfiCallSignalingBody,
    ): Boolean {
        val bytes = try {
            uniffi.lakko_ffi_uniffi.composeCallSignaling(body)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "group call signaling: compose failed", cause)
            return false
        }
        try {
            publishApplicationMessage(groupIdHex = groupIdHex, plaintextBytes = bytes)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "group call signaling: publish failed", cause)
            return false
        }
        me.lakko.app.calls.groupCallSignalOf(body)?.let { signal ->
            applyGroupCallSignal(
                groupIdHex,
                signal.withSender(ownGroupCallMemberKey(), ownGroupCallPersonKey()),
                System.currentTimeMillis(),
            )
        }
        return true
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
            // ADR-0050 — PEOPLE, not leaves: a 1x1 between two people with
            // three devices each is still a 1x1.
            val people = personKeysOf(record.group.members()).size
            people
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "memberCount failed for $groupIdHex", cause)
            null
        }
    }

    /**
     * ADR-0062 — the kind the creator fixed in the group's GroupContext, the
     * same on every member's device: `ONE_TO_ONE` or `GROUP`. `null` when
     * there is no handle, the read fails, or the group carries no kind at all
     * (a `PublisherGroup` / `DeviceGroup`, never a conversation). Lock-free
     * read, like [memberCount]; the kind never changes, so a racing commit
     * cannot make it stale.
     */
    public fun groupKind(groupIdHex: String): GroupKind? {
        val record = groups[groupIdHex] ?: return null
        return try {
            record.group.groupKind()
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "groupKind failed for $groupIdHex", cause)
            null
        }
    }

    /**
     * Which list [groupIdHex] belongs to, read from the group itself
     * ([groupKind], ADR-0062); `null` counts as a group ([conversationKindOf]).
     */
    public fun conversationKind(groupIdHex: String): ConversationKind =
        conversationKindOf(groupKind(groupIdHex))

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
            peopleRows(record.group)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "members failed for $groupIdHex", cause)
            null
        }
    }

    /**
     * ADR-0050 — the roster collapsed to one row per PERSON: `signaturePublicKey`
     * and `accountKey` are the person's account key (what roles, names and
     * authorship key on), `leafIndex` is this device's own leaf for this
     * account's row and the person's first leaf otherwise. Removing a row
     * removes every leaf of the person ([removeMemberFromGroup] fans out).
     */
    private fun peopleRows(group: Group): List<GroupMember> {
        val leaves = group.members()
        val ownLeaf = try {
            group.ownLeafIndex()
        } catch (_: Throwable) {
            null
        }
        val ownPerson = leaves.firstOrNull { it.leafIndex == ownLeaf }?.let { personKeyOf(it) }
        val rows = LinkedHashMap<String, GroupMember>()
        for (leaf in leaves) {
            val person = personKeyOf(leaf)
            val key = outboxB64(person)
            if (rows.containsKey(key)) continue
            val isOwn = ownPerson != null && person.contentEquals(ownPerson)
            rows[key] = GroupMember(
                leafIndex = if (isOwn && ownLeaf != null) ownLeaf else leaf.leafIndex,
                signaturePublicKey = person,
                accountKey = person,
            )
        }
        return rows.values.toList()
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
     * ADR-0048 UX §5 — the other member's signature key in a one-to-one, or
     * `null` for a group (by its ADR-0062 kind, even with two people), an
     * unknown group or an unreadable roster. The
     * chat list names a 1x1 chat after this person.
     */
    public fun oneToOnePeerKey(groupIdHex: String): ByteArray? {
        // ADR-0062 — a two-person GROUP is not a one-to-one: its title, its
        // header and "the chat with this person" never name one member.
        if (groupKind(groupIdHex) != GroupKind.ONE_TO_ONE) return null
        val members = members(groupIdHex) ?: return null
        if (members.size != 2) return null
        val own = ownMemberIdentity(groupIdHex) ?: return null
        return members.firstOrNull { outboxB64(it.signaturePublicKey) != own }?.signaturePublicKey
    }

    /** This PERSON's account key (base64), or `null` before bootstrap. ADR-0059:
     *  the person's own shared list is not a suggestion to themselves. */
    public fun ownPersonKeyB64(): MemberIdentity? =
        canonicalIdentity.get()?.let { runCatching { ownPersonKeyOf(it) }.getOrNull() }

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
    ): MembershipOutcome =
        withContext(mlsDispatcher) { setMemberAdminOnMlsThread(groupIdHex = groupIdHex, memberSignaturePubkey = memberSignaturePubkey, makeAdmin = makeAdmin) }

    private suspend fun setMemberAdminOnMlsThread(
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
                // A delta over the table as it is NOW, never a whole table
                // remembered from earlier: the superseded re-request comes
                // through here too, and a stale table would revert whatever
                // another admin changed in between.
                val admins = adminsWithDelta(table.admins, memberSignaturePubkey, makeAdmin)
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
                            "C-65: a role change is already pending",
                            IllegalStateException("C-65: a role change is already pending"),
                            userText = ROLE_CHANGE_ALREADY_PENDING,
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
                            throw ChatError(dsErrorMessage(e, "DS publishGroup (role-change proposal) failed"), e)
                        }
                        noteOwnRequestBestEffort(groupIdHex, "role-change")
                        noteOwnIntentBestEffort(
                            groupIdHex,
                            RetryableRequest.SetAdmin(outboxB64(memberSignaturePubkey), makeAdmin),
                        )
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome
                    }
                }
                val signerBeforeCommit = currentJournalSigner(groupIdHex)
                val rosterBeforeCommit = rosterLeavesOrNull(record.group)
                val commitBytes = try {
                    record.group.setGroupRoles(identity, table.owner, admins)
                } catch (e: FfiException) {
                    throw ChatError("setGroupRoles failed", e)
                }
                // ADR-0053 client contract point 3 — the Delivery Service learns the
                // key of epoch N+1, proven under N's, BEFORE the outbox entry is
                // written or the commit published. Held inside the provider lock on
                // purpose: the group is eager-merged to an UN-published N+1, and
                // releasing the lock here would let a sibling group's whole-provider
                // snapshot capture it (the ADR-0040 hazard). The PUT is bounded by
                // the DS client's request timeout; a refusal resets the group to the
                // durable N and aborts, exactly as a failed outbox write does.
                val commitSigner = registerNextEpochOrReset(
                    groupIdHex,
                    record.groupId,
                    signerBeforeCommit,
                    rosterBeforeCommit,
                )
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
                        dsClient.publishGroup(record.groupId, commitBytes, signer = commitSigner)
                    } catch (e: Throwable) {
                        throw ChatError(dsErrorMessage(e, "DS publishGroup (role change) failed"), e)
                    }
                    chatClient.markOwnPublishedCommit(groupIdHex, commitPublish.id)
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
    ): MembershipOutcome =
        withContext(mlsDispatcher) { transferGroupOwnershipOnMlsThread(groupIdHex = groupIdHex, newOwnerSignaturePubkey = newOwnerSignaturePubkey, outgoing = outgoing) }

    private suspend fun transferGroupOwnershipOnMlsThread(
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
                        "C-65: a role change is already pending",
                        IllegalStateException("C-65: a role change is already pending"),
                        userText = ROLE_CHANGE_ALREADY_PENDING,
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
                        throw ChatError(dsErrorMessage(e, "DS publishGroup (ownership-transfer proposal) failed"), e)
                    }
                    noteOwnRequestBestEffort(
                        groupIdHex,
                        "role-change",
                        transferToB64 = outboxB64(newOwnerSignaturePubkey),
                    )
                    noteOwnIntentBestEffort(
                        groupIdHex,
                        RetryableRequest.TransferOwnership(outboxB64(newOwnerSignaturePubkey), outgoing),
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
    ): MembershipOutcome =
        withContext(mlsDispatcher) { addMemberToGroupOnMlsThread(groupIdHex = groupIdHex, recipientAccountId = recipientAccountId, recipientSignaturePubkey = recipientSignaturePubkey) }

    private suspend fun addMemberToGroupOnMlsThread(
        groupIdHex: String,
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
    ): MembershipOutcome {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        // ADR-0050 — a person is added as ALL of their devices, each its own
        // leaf: one proposal (or, in a table-less group, one direct commit) per
        // device KeyPackage. The outcome reported is the last device's; every
        // earlier one either landed or threw.
        val devices = discovery.fetchKeyPackages(recipientAccountId)
        if (devices.isNullOrEmpty()) {
            throw ChatError(
                "recipient has no KeyPackage uploaded yet",
                IllegalStateException("fetchKeyPackages returned no device"),
            )
        }
        var outcome: MembershipOutcome? = null
        for (device in devices) {
            outcome = addKeyPackageToGroup(
                groupIdHex,
                identity,
                device.keyPackage,
                skipIfPresent = false,
                accountId = recipientAccountId,
            ) ?: outcome
        }
        outcome = outcome ?: throw ChatError(
            "every device of the recipient is already in the group",
            IllegalStateException("no add performed"),
        )
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
        if (outcome is MembershipOutcome.Committed) {
            redistributePushRoutingBestEffort(groupIdHex)
            // ADR-0048 §2 — the add landed, so the newcomer needs this
            // device's profile. The direct path has no before/after roster
            // diff of its own; a Committed add IS growth.
            profileRepublish.onGrowth(groupIdHex)
        }
        return outcome
    }

    /**
     * ADR-0050 — add ONE device (by its published KeyPackage) to a live group:
     * a proposal in a role-table group (ADR-0045), a direct commit with its
     * Welcome sent to the device's own mailbox otherwise. With [skipIfPresent]
     * the call resolves `null` when a leaf with that signature key is already
     * in the roster (the device fan-outs); otherwise the core refuses a
     * duplicate.
     */
    private suspend fun addKeyPackageToGroup(
        groupIdHex: String,
        identity: Identity,
        keyPackageBytes: ByteArray,
        skipIfPresent: Boolean,
        accountId: ByteArray,
    ): MembershipOutcome? {
        val deviceKey = try {
            uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(keyPackageBytes)
        } catch (e: FfiException) {
            throw ChatError("keyPackageSignatureKey failed", e)
        }
        // G-37 — per-group lock spans the WHOLE op (create-commit, durable
        // record, both publishes), so two membership commits on the SAME group
        // still serialise exactly as before: A is on the wire before B is even
        // created. The provider lock covers only the provider-touching prefix.
        return groupLock(groupIdHex).withLock {
            val publishStep = providerLock.withLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                // Re-checked under the lock: a poll may have applied the very
                // commit that admitted this device while we waited.
                if (skipIfPresent && leafIndexOfSignatureKey(record.group, deviceKey) != null) {
                    return@withLock suspend { null as MembershipOutcome? }
                }
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
                    // ADR-0060 §6 — the committers who may sweep this Add, in
                    // rank order (owner, then admins), so this device can
                    // hand each of them a delegation for the added person's
                    // closed mailbox. Read under the provider lock.
                    val candidates = try {
                        record.group.groupRoles()?.let { roles ->
                            (listOf(roles.owner) + roles.admins)
                                .sortedBy { record.group.committerRank(it)?.toLong() ?: Long.MAX_VALUE }
                        }.orEmpty()
                    } catch (_: Throwable) {
                        emptyList()
                    }
                    val addedAccount = try {
                        uniffi.lakko_ffi_uniffi.keyPackageAccountKey(keyPackageBytes)
                    } catch (_: Throwable) {
                        null
                    } ?: deviceKey
                    return@withLock suspend {
                        val publish = try {
                            dsClient.publishGroup(record.groupId, proposalBytes)
                        } catch (e: Throwable) {
                            throw ChatError(dsErrorMessage(e, "DS publishGroup (add-member proposal) failed"), e)
                        }
                        noteOwnRequestBestEffort(groupIdHex, "add:" + outboxB64(deviceKey))
                        // ADR-0050 section 2.5a — per DEVICE, with the account
                        // id the KeyPackage is fetched again by: a KeyPackage
                        // is single-use, so a re-request never re-sends this one.
                        noteOwnIntentBestEffort(
                            groupIdHex,
                            RetryableRequest.Add(outboxB64(deviceKey), outboxB64(accountId)),
                        )
                        // Launched: the broadcast takes the per-group lock this
                        // step is still inside.
                        sendCommitterDelegationsBestEffort(groupIdHex, addedAccount, candidates)
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome?
                    }
                }
                // 1. create-commit — eager-merges epoch N -> N+1 in the in-memory provider.
                val signerBeforeCommit = currentJournalSigner(groupIdHex)
                val rosterBeforeCommit = rosterLeavesOrNull(record.group)
                val addResult = try {
                    record.group.addMemberFromWire(identity, keyPackageBytes)
                } catch (e: FfiException) {
                    throw ChatError("addMemberFromWire failed", e)
                }
                // ADR-0053 client contract point 3 — the Delivery Service learns the
                // key of epoch N+1, proven under N's, BEFORE the outbox entry is
                // written or the commit published. Held inside the provider lock on
                // purpose: the group is eager-merged to an UN-published N+1, and
                // releasing the lock here would let a sibling group's whole-provider
                // snapshot capture it (the ADR-0040 hazard). The PUT is bounded by
                // the DS client's request timeout; a refusal resets the group to the
                // durable N and aborts, exactly as a failed outbox write does.
                val commitSigner = registerNextEpochOrReset(
                    groupIdHex,
                    record.groupId,
                    signerBeforeCommit,
                    rosterBeforeCommit,
                )
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
                    // ADR-0060 §6 (amending ADR-0050 §2.7): the Welcome goes to
                    // the added leaf's ACCOUNT mailbox, resolved through the
                    // leaf's device binding; every device of that account reads
                    // it, and only this leaf's KeyPackage opens it.
                    val recipientAccount = try {
                        uniffi.lakko_ffi_uniffi.keyPackageAccountKey(keyPackageBytes)
                    } catch (_: Throwable) {
                        null
                    } ?: deviceKey
                    val recipientPseudonym = welcomeMailboxFor(recipientAccount)
                    val entry = PendingCommitEntry(
                        id = newOutboxEntryId(),
                        accountId = requireAccountId(),
                        groupId = outboxB64(record.groupId),
                        targetEpoch = record.group.epoch().toString(),
                        commitB64 = outboxB64(addResult.commit),
                        secondArtifactB64 = outboxB64(welcomeEnvelope),
                        secondArtifactKind = SecondArtifactKind.WELCOME,
                        secondArtifactMailboxB64 = outboxB64(recipientPseudonym),
                        secondArtifactAccountKeyB64 = outboxB64(recipientAccount),
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
                        dsClient.publishGroup(record.groupId, addResult.commit, signer = commitSigner)
                    } catch (e: Throwable) {
                        throw ChatError(dsErrorMessage(e, "DS publishGroup (add-member commit) failed"), e)
                    }
                    chatClient.markOwnPublishedCommit(groupIdHex, commitPublish.id)
                    val welcomePublish = try {
                        publishWelcomeToMailbox(recipientPseudonym, welcomeEnvelope, groupIdHex)
                    } catch (e: Throwable) {
                        throw ChatError(dsErrorMessage(e, "DS publishMailbox (Welcome) failed"), e)
                    }
                    // 4. both artifacts on the wire -> ack (best-effort).
                    ackCommitBestEffort(entry.id, commitPublish.id, welcomePublish.id)
                    publishThreadList()
                    MembershipOutcome.Committed(commitPublish.id, welcomePublish.id) as MembershipOutcome?
                }
            }
            publishStep()
        }
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
    ): MembershipOutcome =
        withContext(mlsDispatcher) { removeMemberFromGroupOnMlsThread(groupIdHex = groupIdHex, leafIndex = leafIndex) }

    private suspend fun removeMemberFromGroupOnMlsThread(
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
                // ADR-0050 — removing a member removes the PERSON: every leaf of
                // the account the named leaf belongs to (leaving removes every
                // own device). Leaves are re-resolved by key before each step
                // because a direct commit renumbers the tree.
                val targets = groups[groupIdHex]?.let { record ->
                    providerLock.withLock {
                        val leaves = leavesOfSameAccount(record.group, leafIndex)
                        val roster = rosterOf(record.group)
                        leaves.mapNotNull { leaf ->
                            roster.firstOrNull { it.leafIndex == leaf }?.signaturePublicKey
                        }
                    }
                }.orEmpty()
                if (targets.size <= 1) {
                    publishRemoveCommit(groupIdHex, identity, leafIndex).also {
                        removalLanded = it is MembershipOutcome.Committed
                    }
                } else {
                    var last: MembershipOutcome? = null
                    for (key in targets) {
                        val leaf = groups[groupIdHex]?.let { record ->
                            providerLock.withLock { leafIndexOfSignatureKey(record.group, key) }
                        } ?: continue
                        last = publishRemoveCommit(groupIdHex, identity, leaf).also {
                            if (it is MembershipOutcome.Committed) removalLanded = true
                        }
                    }
                    last ?: throw ChatError(
                        "no leaf of that member is in the group any more",
                        IllegalStateException("remove targets vanished"),
                    )
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
                    // ADR-0050 section 2.5a — the subject by SIGNATURE KEY, read
                    // before the proposal: a leaf is only a position, and the
                    // re-request must find the same person after a reuse.
                    val subjectKey = try {
                        rosterOf(record.group).firstOrNull { it.leafIndex == leafIndex }?.signaturePublicKey
                    } catch (_: Throwable) {
                        null
                    }
                    val proposalBytes = try {
                        record.group.proposeRemoveMember(identity, leafIndex)
                    } catch (e: FfiException) {
                        throw ChatError("proposeRemoveMember failed", e)
                    }
                    persistSnapshotBestEffort()
                    val intent = subjectKey?.let { key ->
                        val keyB64 = outboxB64(key)
                        if (key.contentEquals(identity.signaturePublicKey())) {
                            RetryableRequest.Leave(keyB64)
                        } else {
                            RetryableRequest.Remove(keyB64)
                        }
                    }
                    return@withLock suspend {
                        val publish = try {
                            dsClient.publishGroup(record.groupId, proposalBytes)
                        } catch (e: Throwable) {
                            throw ChatError(dsErrorMessage(e, "DS publishGroup (remove-member proposal) failed"), e)
                        }
                        noteOwnRequestBestEffort(groupIdHex, "remove:$leafIndex")
                        intent?.let { noteOwnIntentBestEffort(groupIdHex, it) }
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome
                    }
                }
                val signerBeforeCommit = currentJournalSigner(groupIdHex)
                val rosterBeforeCommit = rosterLeavesOrNull(record.group)
                val commitBytes = try {
                    record.group.removeMember(identity, leafIndex)
                } catch (e: FfiException) {
                    throw ChatError("removeMember failed", e)
                }
                // ADR-0053 client contract point 3 — the Delivery Service learns the
                // key of epoch N+1, proven under N's, BEFORE the outbox entry is
                // written or the commit published. Held inside the provider lock on
                // purpose: the group is eager-merged to an UN-published N+1, and
                // releasing the lock here would let a sibling group's whole-provider
                // snapshot capture it (the ADR-0040 hazard). The PUT is bounded by
                // the DS client's request timeout; a refusal resets the group to the
                // durable N and aborts, exactly as a failed outbox write does.
                val commitSigner = registerNextEpochOrReset(
                    groupIdHex,
                    record.groupId,
                    signerBeforeCommit,
                    rosterBeforeCommit,
                )
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
                        dsClient.publishGroup(record.groupId, commitBytes, signer = commitSigner)
                    } catch (e: Throwable) {
                        throw ChatError(dsErrorMessage(e, "DS publishGroup (remove-member commit) failed"), e)
                    }
                    chatClient.markOwnPublishedCommit(groupIdHex, commitPublish.id)
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

    /**
     * ADR-0053 — the ACCOUNT key behind a pending join request keyed on
     * [joinerSignatureKey] (a device leaf key): from the joiner's verified
     * device binding, or the joiner's own key when it is unbound. `null` when
     * there is no such pending request or the handle cannot be read. For the
     * label only: approve and reject still take the leaf key.
     */
    public fun pendingExternalJoinAccountKey(groupIdHex: String, joinerSignatureKey: ByteArray): ByteArray? {
        val record = groups[groupIdHex] ?: return null
        return try {
            record.group.pendingExternalJoinAccountKey(joinerSignatureKey)
        } catch (e: Throwable) {
            null
        }
    }

    /**
     * ADR-0045 UX §7 — the same retained set, but with a TORN read reported as
     * `null` rather than folded into an empty list.
     *
     * A panel wants the fold (nothing to render either way). The retry verdict
     * does not: "no request retained" is a reason to DROP an armed approve, and
     * a failed read must not be allowed to say that.
     */
    public fun retainedExternalJoinKeys(groupIdHex: String): List<ByteArray>? {
        val record = groups[groupIdHex] ?: return null
        return try {
            record.group.pendingExternalJoins()
        } catch (e: FfiException) {
            android.util.Log.w("ChatRepository", "retainedExternalJoinKeys failed", e)
            null
        }
    }

    /**
     * ADR-0027 / UX §7.1 — whether a request the quarantine turned away has
     * been observed for this group since the last time an admin acted.
     *
     * Not derivable from [pendingExternalJoins]: an unkept request is
     * precisely the one that never entered that set.
     */
    public fun sawUnkeptExternalJoin(groupIdHex: String): Boolean =
        groups[groupIdHex]?.sawUnkeptExternalJoin ?: false

    /**
     * Forget the unkept-request observation, because acting on the queue is
     * what may have made room for the request it turned away.
     */
    public fun clearUnkeptExternalJoin(groupIdHex: String) {
        groups[groupIdHex]?.sawUnkeptExternalJoin = false
    }

    /** ADR-0028 OQ2 (G-3) — advisory attribution for a pending request: which
     *  member minted the `/g/` link it arrived on, and whether that issuer is in
     *  the current roster. */
    public data class JoinAttributionInfo(
        val joinerSignatureKey: ByteArray,
        val issuerFingerprint: String,
        val issuerInRoster: Boolean,
        /** ADR-0048 — the issuer's signature key, so the UI can show a name. */
        val issuerSignatureKey: ByteArray = ByteArray(0),
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
    public suspend fun fetchJoinAttributions(groupIdHex: String): List<JoinAttributionInfo> =
        withContext(mlsDispatcher) { fetchJoinAttributionsOnMlsThread(groupIdHex = groupIdHex) }

    private suspend fun fetchJoinAttributionsOnMlsThread(groupIdHex: String): List<JoinAttributionInfo> {
        val record = groups[groupIdHex] ?: return emptyList()
        val groupId = try {
            record.group.groupId()
        } catch (e: Throwable) {
            return emptyList()
        }
        val rosterKeys = try {
            // ADR-0050: an invite issuer signs with the account key, so the
            // roster check matches both leaf and person keys.
            record.group.members().flatMap { listOf(it.signaturePublicKey, personKeyOf(it)) }
        } catch (e: Throwable) {
            emptyList()
        }
        // ADR-0053 section 4 — an attribution arrives with its proposal in a
        // knock, never in the journal.
        val raw = try {
            dsClient.fetchKnocks(groupId, after = 0L, waitSecs = 0)
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
                    issuerSignatureKey = attr.issuerSignaturePubkey,
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
     * The joiner's Welcome is produced by whichever device later sweeps this
     * proposal into a commit. See [runCommitterSweep]: since ADR-0053 that
     * sweep sends it to each added key's mailbox only, blind to which of them
     * came from a link.
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
    ): Long =
        withContext(mlsDispatcher) { approveExternalJoinOnMlsThread(groupIdHex = groupIdHex, proposalRef = proposalRef) }

    private suspend fun approveExternalJoinOnMlsThread(
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
                throw ChatError(dsErrorMessage(e, "DS publishGroup (approve proposal) failed"), e)
            }
            chatClient.markOwnPublishedCommit(groupIdHex, publish.id)
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
            noteOwnIntentBestEffort(groupIdHex, RetryableRequest.ApproveJoin(outboxB64(proposalRef)))
            publishThreadList()
            publish.id
        }
    }

    /**
     * ADR-0027 (G-3) — reject a pending external-join request (owner/admin only).
     * Drops the retained proposal so it leaves the pending list. No commit, no
     * journal post — the joiner never receives a Welcome. The retained set is
     * persisted inside the MLS storage snapshot, so the rejection is written to
     * the durable copy and the following `persistSnapshotBestEffort()` is what
     * makes it survive a restart — without it the request would be back in the
     * pending list the next time the group is loaded.
     */
    public suspend fun rejectExternalJoin(
        groupIdHex: String,
        proposalRef: ByteArray,
    ): Unit =
        withContext(mlsDispatcher) { rejectExternalJoinOnMlsThread(groupIdHex = groupIdHex, proposalRef = proposalRef) }

    private suspend fun rejectExternalJoinOnMlsThread(
        groupIdHex: String,
        proposalRef: ByteArray,
    ): Unit {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        // G-37 — local-only (no DS call): provider work start to finish.
        groupLock(groupIdHex).withLock {
            providerLock.withLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                try {
                    record.group.rejectExternalJoin(identity, proposalRef)
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
    public suspend fun createGroupInviteLink(groupIdHex: String): String =
        withContext(mlsDispatcher) { createGroupInviteLinkOnMlsThread(groupIdHex = groupIdHex) }

    private suspend fun createGroupInviteLinkOnMlsThread(groupIdHex: String): String {
        // ADR-0050: an invite capability is the person's, signed by the account key.
        val identity = accountIdentity.get() ?: canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("missing group record"),
            )
        // ADR-0028 v2 (docs/ux/group-invite-page.md) — the link carries a
        // sealed preview: the group's shared name, this device's own profile
        // name and the roster size at mint time, all already on the device.
        // Sealed under a key derived from a fresh 12-byte link secret that
        // goes into the URL fragment and nowhere a server reads. The core trims the names and reads a blank
        // one as absent.
        val groupName = groupName(groupIdHex)?.name
        val inviterName = ownDisplayName()
        val members = (memberCount(groupIdHex) ?: 0).coerceIn(0, UShort.MAX_VALUE.toInt()).toUShort()
        val seed = identity.ed25519Seed()
        val groupId = record.group.groupId()
        val mint = try {
            uniffi.lakko_ffi_uniffi.composeGroupInviteCapabilityWithPreview(
                seed,
                groupId,
                0u,
                groupName,
                inviterName,
                members,
            )
        } catch (e: FfiException) {
            // A stored name that fails the display-name rule (it should not:
            // both were gated by that rule when stored). The link must still
            // mint; it carries the count and no names rather than no link.
            android.util.Log.w("ChatRepository", "invite preview refused a name; minting without names", e)
            try {
                uniffi.lakko_ffi_uniffi.composeGroupInviteCapabilityWithPreview(
                    seed,
                    groupId,
                    0u,
                    null,
                    null,
                    members,
                )
            } catch (again: FfiException) {
                throw ChatError("composeGroupInviteCapabilityWithPreview failed", again)
            }
        }
        // Sealed links (2026-09-28): the ledger stores only the capability
        // sealed under the link secret; the secret rides in the fragment.
        // Production default (ADR-0028 OQ1): upload to the discovery ledger --
        // the server-side anti-abuse surface (per-joiner consume dedup + soft
        // max_uses) -- and share the short pointer URL. The upload is
        // bearer-gated, so it runs on a client carrying the session token read
        // NOW; no session is a loud failure. Only a transport failure (a
        // censored / offline network) falls back to the inline form over the
        // sealed bytes; a server refusal is thrown. `max_uses` is a soft
        // throttle, not an access control: the admin approval gate stays the
        // boundary. Either form carries the fragment; `Copy link` copies the
        // whole string.
        val upload = me.lakko.app.discovery.SealedInviteUpload(
            sealed = mint.sealed,
            issuerPubkey = mint.issuerPubkey,
            expiryMs = 0L,
            maxUses = 0xFFFFFFFFL,
        )
        val blobId = uploadGroupInviteOrNull(
            sessionToken = keyPackageAccount.get()?.sessionToken,
            clientFactory = keyPackageDiscoveryFactory,
            upload = { client -> client.uploadGroupInvite(upload) },
            log = { message -> android.util.Log.w("ChatRepository", message) },
        )
        return me.lakko.app.discovery.composeSealedInviteUrl(
            origin = GROUP_INVITE_DEFAULT_ORIGIN,
            prefix = GROUP_INVITE_URL_PREFIX,
            blobId = blobId,
            sealed = mint.sealed,
            linkSecret = mint.linkSecret,
        )
    }

    /**
     * ADR-0028 revoke — the issuer revokes a previously-created group invite
     * link. Only the server-backed pointer form (a 6-byte ledger id) is
     * revocable; an inline link has no ledger row. Returns `true` when a ledger
     * row was revoked, `false` for an inline / malformed link (nothing to do).
     */
    public suspend fun revokeGroupInviteLink(link: String): Boolean =
        withContext(mlsDispatcher) { revokeGroupInviteLinkOnMlsThread(link = link) }

    private suspend fun revokeGroupInviteLinkOnMlsThread(link: String): Boolean {
        val identity = accountIdentity.get() ?: canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        // The slug ends before the `#<link key>` fragment (ADR-0028 v2).
        val slug = groupInviteSlugOf(link) ?: return false
        val parsed = try {
            parseGroupInviteSlug(slug)
        } catch (e: GroupInviteUrlException) {
            return false
        }
        return when (parsed) {
            is ParsedGroupInvite.Inline -> false
            is ParsedGroupInvite.Pointer -> {
                // Bearer-gated route: a client carrying the session token
                // read now (a missing session throws a ChatError as is).
                withSessionDiscovery(
                    sessionToken = keyPackageAccount.get()?.sessionToken,
                    what = "group invite revoke",
                    clientFactory = keyPackageDiscoveryFactory,
                ) { client ->
                    try {
                        client.revokeGroupInvite(parsed.blobId, identity.ed25519Seed())
                    } catch (e: Throwable) {
                        throw ChatError("group invite revoke failed", e)
                    }
                }
                true
            }
        }
    }

    /**
     * ADR-0028 (G-3) — resolve a `/g/<slug>` slug to the SEALED link bytes,
     * fetching from the discovery ledger when the slug is a 6-byte ledger-id
     * pointer (the production form) or passing the bytes
     * straight through when it is an inline sealed link (the censored-region
     * fallback). Returns the sealed bytes + the `blobId` (non-null only for the
     * pointer form) so the caller can open them under the fragment
     * ([inspectGroupInvite]) and drive the consume gate before requesting to
     * join.
     */
    public suspend fun resolveGroupInviteSlug(slug: String): Pair<ByteArray, ByteArray?> =
        withContext(mlsDispatcher) { resolveGroupInviteSlugOnMlsThread(slug = slug) }

    private suspend fun resolveGroupInviteSlugOnMlsThread(slug: String): Pair<ByteArray, ByteArray?> {
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
     * What the group invite page shows before anyone taps (ADR-0028 v2 and the
     * sealed-link amendment, `docs/ux/group-invite-page.md` section 3). Opens
     * the [sealed] link under the secret in [fragment], verifies the
     * capability, checks expiry at [nowMs], and opens the capability's preview
     * when it carries one.
     *
     * A missing or malformed fragment, sealed bytes that do not open under it
     * (a wrong secret, a tampered link, a blob a server swapped in under a live
     * id), or a present preview that does not open, all throw
     * [GroupInviteLinkDamaged] — fail closed, never a preview-less fallback.
     * A capability minted without a preview opens and shows no preview.
     *
     * @throws GroupInviteLinkDamaged when the link is damaged.
     */
    public suspend fun inspectGroupInvite(
        sealed: ByteArray,
        fragment: GroupInviteFragment,
        nowMs: ULong,
    ): GroupInviteCard = withContext(mlsDispatcher) {
        val secret = fragment.secretOrNull() ?: run {
            android.util.Log.w("ChatRepository", "invite link has no usable fragment; refusing the link")
            throw GroupInviteLinkDamaged()
        }
        val cap = try {
            me.lakko.app.discovery.openSealedInviteLink(sealed, secret) { bytes, key ->
                uniffi.lakko_ffi_uniffi.GroupInviteCapability.openLink(bytes, key)
            }
        } catch (e: me.lakko.app.discovery.InviteLinkDamaged) {
            android.util.Log.w("ChatRepository", "invite link does not open under its fragment; refusing the link")
            throw GroupInviteLinkDamaged().also { it.initCause(e) }
        }
        try {
            val outcome = resolveGroupInvitePreview(cap.hasPreview(), secret) { key ->
                cap.openPreview(key)?.let {
                    GroupInvitePreviewUi(
                        groupName = it.groupName,
                        inviterName = it.inviterName,
                        memberCount = it.memberCount.toInt(),
                    )
                }
            }
            when (outcome) {
                GroupInvitePreviewOutcome.Damaged -> {
                    android.util.Log.w("ChatRepository", "invite link preview does not open; refusing the link")
                    throw GroupInviteLinkDamaged()
                }
                is GroupInvitePreviewOutcome.Shown -> GroupInviteCard(
                    fingerprint = cap.issuerFingerprint(),
                    valid = cap.isValidNow(nowMs),
                    preview = outcome.preview,
                    capabilityWire = cap.wire(),
                )
            }
        } finally {
            cap.destroy()
        }
    }

    /**
     * ADR-0028 (G-3) — joiner step 1: request to join a group from a `/g/` link
     * capability (the PLAINTEXT wire bytes [inspectGroupInvite] opened). Decodes +
     * verifies the capability fail-closed (before any post), then knocks
     * (ADR-0053 §4): an empty knock learns the group's epoch, a second one
     * carries the attribution and the external-join proposal (ADR-0027) built
     * for it. The journal is never read. Returns the group id + the
     * joiner's signature key — the caller drives [pollForJoinWelcome] until an
     * admin approves and the Welcome arrives.
     */
    public suspend fun requestToJoinFromLink(
        capabilityWire: ByteArray,
        nowMs: ULong,
        blobId: ByteArray? = null,
    ): Pair<ByteArray, ByteArray> =
        withContext(mlsDispatcher) { requestToJoinFromLinkOnMlsThread(capabilityWire = capabilityWire, nowMs = nowMs, blobId = blobId) }

    private suspend fun requestToJoinFromLinkOnMlsThread(
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
        val joinReplyGrant = try {
            me.lakko.app.reach.ReachManager.get(applicationContext).mintReply(toKey = groupId)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            throw ChatError(dsErrorMessage(e, "Could not prepare the request. Try again."), e)
        }
        // ADR-0053 section 4 / client contract point 6 — a claimed journal
        // answers only its members, so the joiner never reads it: an empty
        // knock learns the epoch, and a second knock carries the attribution
        // (advisory, ADR-0028 OQ2) and the proposal built for that epoch. Each
        // knock spends its own `ds-knock` token (ADR-0054).
        try {
            runLinkJoinKnock(
                knock = { payloads ->
                    dsClient.knock(groupId, payloads).epoch
                },
                buildProposal = { epoch ->
                    providerLock.withLock {
                        val proposal = identity.createExternalJoinProposal(groupId, epoch)
                        // The proposal's KeyPackage private key is the only thing
                        // that can open the Welcome the sweep will send later;
                        // persist it now rather than losing it to a process death.
                        persistSnapshotBestEffort()
                        proposal
                    }
                },
                buildAttribution = {
                    uniffi.lakko_ffi_uniffi.composeGroupJoinAttribution(
                        identity.ed25519Seed(),
                        capabilityWire,
                        // ADR-0060 §6 — this account's reply key, registered on
                        // its mailbox before the knock, so the committer can
                        // write the Welcome in. Retired once the group arrives.
                        joinReplyGrant,
                    )
                },
            )
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: GroupNotAcceptingRequests) {
            throw ChatError(GROUP_NOT_ACCEPTING_REQUESTS, e)
        } catch (e: FfiException) {
            throw ChatError("createExternalJoinProposal failed", e)
        } catch (e: Throwable) {
            throw ChatError(dsErrorMessage(e, "DS knock (external-join request) failed"), e)
        }
        return Pair(groupId, identity.signaturePublicKey())
    }

    /**
     * ADR-0028 (G-3) joiner step 2, on the ADR-0053 route: the Welcome the
     * committer sweep sends after an admin approves arrives in THIS device's
     * mailbox (the pseudonym of the leaf key the proposal carried), where the
     * ordinary device-mailbox handler joins it. So the wait is for the group to
     * appear, driven by one mailbox pass; the group journal is never read.
     * Returns the joined group's hex id, or `null` while nothing has arrived
     * (keep polling). [joinerSignatureKey] is kept for the caller's shape; the
     * mailbox is addressed by this device's own key.
     */
    public suspend fun pollForJoinWelcome(
        groupId: ByteArray,
        joinerSignatureKey: ByteArray,
    ): String? =
        withContext(mlsDispatcher) { pollForJoinWelcomeOnMlsThread(groupId = groupId) }

    private suspend fun pollForJoinWelcomeOnMlsThread(groupId: ByteArray): String? {
        // ADR-0060 §6 — the Welcome arrives in this ACCOUNT's mailbox, whose
        // one continuous reader (the app shell's inbox long poll) joins it; a
        // second reader here would race that reader's cursor. So the wait is
        // for the group to appear.
        val groupIdHex = DsClient.pseudonymHex(groupId)
        return if (groups.containsKey(groupIdHex)) {
            me.lakko.app.reach.ReachManager.get(applicationContext).retireRepliesTo(groupId)
            groupIdHex
        } else {
            null
        }
    }

    /**
     * Publish a raw application payload — a call-signaling frame, a service
     * envelope — with no chat bubble and no message log entry.
     *
     * Fires a push wake for exactly the two call-signaling frames
     * [me.lakko.app.calls.shouldWakeForCallSignaling] names (an offer and an
     * end) and for nothing else, so a backgrounded or closed callee rings
     * (`docs/ux/calls.md` §2.1 / §7). The decision is taken from the composed
     * bytes here rather than passed in by the caller, so there is no second
     * place a caller could get it wrong: the group-call control frames that
     * reach this same method through [publishGroupCallSignal] stay silent
     * because their tag is not an offer or an end, not because someone
     * remembered to say so.
     */
    public suspend fun publishApplicationMessage(
        groupIdHex: String,
        plaintextBytes: ByteArray,
    ): Unit =
        withContext(mlsDispatcher) { publishApplicationMessageOnMlsThread(groupIdHex = groupIdHex, plaintextBytes = plaintextBytes) }

    private suspend fun publishApplicationMessageOnMlsThread(
        groupIdHex: String,
        plaintextBytes: ByteArray,
    ): Unit {
        // UX 16.4 — nothing is ever sent into a closed one-to-one.
        if (closedOneToOne(groupIdHex)) throw closedOneToOneError()
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
            // docs/ux/calls.md §2.1 — a call must arrive when the app is closed.
            // ADR-0054 — the wake rides this signed publish as a header and the
            // DS fans it out after appending the row; only an offer and an end
            // ask for one, every other frame published here stays silent.
            val wake = DsWake.forPublish(
                pushGid = pushGidForGroupOrNull(groupIdHex),
                kind = if (me.lakko.app.calls.shouldWakeForCallSignaling(plaintextBytes)) DsWakeKind.CALL else null,
            )
            // ADR-0045 — the publish is INSIDE the per-group lock (see
            // [runGroupSend]): a message sealed at one epoch and published on the
            // far side of a commit is permanently undecryptable for every
            // recipient, because `lakko-mls` retains no past-epoch secrets.
            chatClient.publishPreparedSend(prepared, wake = wake)
        }
    }

    public suspend fun sendMessage(
        groupIdHex: String,
        plaintext: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
        /** ADR-0052 — the quote this message carries, when it is a reply. */
        reply: ReplyRef? = null,
        /** ADR-0052 decision 2 — the attribution this message carries, when it
         *  is a forward. A forward never carries a quote, so the two are not
         *  set together. */
        forward: ForwardRef? = null,
    ): Unit =
        withContext(mlsDispatcher) {
            sendMessageOnMlsThread(
                groupIdHex = groupIdHex,
                plaintext = plaintext,
                attachments = attachments,
                reply = reply,
                forward = forward,
            )
        }

    private suspend fun sendMessageOnMlsThread(
        groupIdHex: String,
        plaintext: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
        reply: ReplyRef? = null,
        forward: ForwardRef? = null,
    ): Unit {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        // ADR-0065 phase 2 / UX 16.4 — nothing is ever sent into a closed
        // one-to-one: a message here would reach the old devices, which is
        // exactly what a reset exists to end. The composer is replaced, so
        // this is defence in depth behind the UI.
        if (closedOneToOne(groupIdHex)) throw closedOneToOneError()
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )

        // The row goes into the conversation BEFORE the publish, reading
        // "Sending...". It used to be appended only on success, so a message
        // written on a bad connection left the person looking at an error
        // over an unchanged composer with nothing in the conversation - the
        // words were safe but the message was not there. On failure the row
        // stays and says "Not sent" with a Retry beside it
        // (`docs/ux/media-messages.md` section 3), which is the same contract
        // the web and iOS clients keep.
        val localId = record.nextLocalId()
        record.appendPending(
            localId = localId,
            text = plaintext,
            attachments = attachments,
            createdAt = java.time.Instant.now().toString(),
            authorKey = null,
            reply = reply,
            forward = forward,
        )
        publishThreadList()
        try {
            sendLocked(
                groupIdHex = groupIdHex,
                record = record,
                identity = identity,
                localId = localId,
                plaintext = plaintext,
                attachments = attachments,
                reply = reply,
                forward = forward,
            )
        } catch (e: Throwable) {
            record.markFailed(localId)
            publishThreadList()
            // Deliberately NOT persisted: an unsent row is not written to the
            // message log, so a relaunch is where a failed send stops being
            // retryable. Making that survive is a durable outbox, which no
            // client has.
            throw e
        }
    }

    /**
     * Send a row that did not get there the first time.
     *
     * Re-publishes the envelope from the row itself, so the words the person
     * wrote are the words that go - they were never anywhere else to be
     * fetched from.
     *
     * **It can deliver the message twice.** A publish that reached the server
     * and lost its answer is indistinguishable here from one that never
     * arrived: the send is not idempotent, there is no key to de-duplicate
     * on, and our own message is one the MLS ratchet will not decrypt a
     * second time, so the journal cannot be consulted either. The same is
     * true of a person re-typing a message by hand, which is what they did
     * before this existed; a button makes it cheaper to reach, not newly
     * possible. Losing the message is the worse failure.
     */
    public suspend fun retrySend(groupIdHex: String, localId: Long): Unit =
        withContext(mlsDispatcher) { retrySendOnMlsThread(groupIdHex, localId) }

    private suspend fun retrySendOnMlsThread(groupIdHex: String, localId: Long) {
        // UX 16.4 — nothing is ever sent into a closed one-to-one.
        if (closedOneToOne(groupIdHex)) throw closedOneToOneError()
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        val row = record.snapshot().firstOrNull { it.localId == localId } ?: return
        if (row.ack != Ack.FAILED) return
        record.markPending(localId)
        publishThreadList()
        try {
            sendLocked(
                groupIdHex = groupIdHex,
                record = record,
                identity = identity,
                localId = localId,
                plaintext = row.text,
                attachments = row.attachments,
                // The quote goes again with the words: the row is the only
                // place either was ever kept. The forward attribution too.
                reply = row.reply,
                forward = row.forward,
            )
        } catch (e: Throwable) {
            record.markFailed(localId)
            publishThreadList()
            throw e
        }
    }

    /** The publish itself, under this group's lock. */
    private suspend fun sendLocked(
        groupIdHex: String,
        record: GroupRecord,
        identity: Identity,
        localId: Long,
        plaintext: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef>,
        reply: ReplyRef? = null,
        forward: ForwardRef? = null,
    ) {
        groupLock(groupIdHex).withLock {
            // 1.19c4: build the JSON envelope before
            // encrypting. `composeEnvelopeBytes` routes through
            // the UniFFI `composeEnvelope` free function;
            // wrong-length byte fields are rejected there
            // (defence-in-depth: the UI already validates the
            // AttachmentRefs it builds via
            // `AttachmentBridge.encryptAndUpload`).
            // ADR-0052 — a reply is the same envelope with `reply_to` and the
            // snapshot beside the text; the core bounds the excerpt and drops
            // an empty string, so what is passed is what the spec says.
            // ADR-0052 decision 2 — a forward is the same envelope with the
            // original author's name and the origin pair; the core truncates
            // the name and drops an over-long origin. The origin group id
            // crosses as STANDARD base64 of the raw id, while the app keys
            // groups on hex, so the conversion happens here, once.
            val envelopeBytes = try {
                if (forward != null) {
                    uniffi.lakko_ffi_uniffi.composeForwardEnvelope(
                        text = plaintext,
                        attachments = attachments,
                        forwardedFrom = forward.author,
                        originGroupIdB64 = forward.originGroupIdHex?.let(::forwardOriginGroupIdB64From),
                        originMessageId = forward.originMessageId,
                    )
                } else if (reply != null) {
                    uniffi.lakko_ffi_uniffi.composeReplyEnvelope(
                        text = plaintext,
                        attachments = attachments,
                        replyTo = reply.target,
                        replyAuthor = reply.author,
                        replyExcerpt = reply.excerpt,
                    )
                } else {
                    AttachmentBridge.composeEnvelopeBytes(
                        text = plaintext,
                        attachments = attachments,
                    )
                }
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
            // ADR-0054 — the wake to every device subscribed to this group
            // rides the signed publish as the `x-lakko-wake` header; the DS
            // forwards it to push-gw after appending the row. ADR-0042 — it
            // names the opaque `push_gid`, never the DS `group_id`; with no
            // routing secret held yet the header is omitted and recipients
            // pick the message up on their next foreground poll.
            val wake = DsWake.forPublish(
                pushGid = pushGidForGroupOrNull(groupIdHex),
                kind = DsWakeKind.MESSAGE,
            )
            val result = chatClient.publishPreparedSend(prepared, wake = wake)
            record.markSent(
                localId = localId,
                messageId = result.messageId,
                createdAt = result.createdAt,
                authorKey = ownKey,
            )
            persistMessageLogBestEffort()
            publishThreadList()
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
    ): Unit =
        withContext(mlsDispatcher) { editMessageOnMlsThread(groupIdHex = groupIdHex, targetMessageId = targetMessageId, newText = newText) }

    private suspend fun editMessageOnMlsThread(
        groupIdHex: String,
        targetMessageId: Long,
        newText: String,
    ): Unit {
        // UX 16.4 — nothing is ever sent into a closed one-to-one.
        if (closedOneToOne(groupIdHex)) throw closedOneToOneError()
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
    ): Unit =
        withContext(mlsDispatcher) { deleteMessageOnMlsThread(groupIdHex = groupIdHex, targetMessageId = targetMessageId) }

    private suspend fun deleteMessageOnMlsThread(
        groupIdHex: String,
        targetMessageId: Long,
    ): Unit {
        // UX 16.4 — nothing is ever sent into a closed one-to-one.
        if (closedOneToOne(groupIdHex)) throw closedOneToOneError()
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
            // ADR-0052 decision 3 — the deleted message's pins go with it.
            dropPinsOfDeletedMessages(groupIdHex, record)
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
    ): Unit =
        withContext(mlsDispatcher) { reactToMessageOnMlsThread(groupIdHex = groupIdHex, targetMessageId = targetMessageId, emoji = emoji) }

    private suspend fun reactToMessageOnMlsThread(
        groupIdHex: String,
        targetMessageId: Long,
        emoji: String,
    ): Unit {
        // UX 16.4 — nothing is ever sent into a closed one-to-one.
        if (closedOneToOne(groupIdHex)) throw closedOneToOneError()
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
    public suspend fun sendTyping(groupIdHex: String, isTyping: Boolean): Unit =
        withContext(mlsDispatcher) { sendTypingOnMlsThread(groupIdHex = groupIdHex, isTyping = isTyping) }

    private suspend fun sendTypingOnMlsThread(groupIdHex: String, isTyping: Boolean): Unit {
        // UX 16.4 — nothing is ever sent into a closed one-to-one.
        if (closedOneToOne(groupIdHex)) return
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
    public suspend fun sendReadReceipt(groupIdHex: String, upTo: Long): Unit =
        withContext(mlsDispatcher) { sendReadReceiptOnMlsThread(groupIdHex = groupIdHex, upTo = upTo) }

    private suspend fun sendReadReceiptOnMlsThread(groupIdHex: String, upTo: Long): Unit {
        // UX 16.4 — nothing is ever sent into a closed one-to-one.
        if (closedOneToOne(groupIdHex)) return
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
            // ADR-0057 obligation 8 — bind the journal to a freshly minted or
            // rotated push_gid, with the root or an admin's registrar key.
            bindFn = { pushGid ->
                val groupId = runCatching { hexToBytes(groupIdHex) }.getOrNull()
                if (groupId != null) {
                    bindPushGidBestEffort(groupIdHex, group, groupId, pushGid)
                } else {
                    me.lakko.app.ds.PushBindingOutcome.FAILED
                }
            },
        )

    // -----------------------------------------------------------------
    // ADR-0048 — profile names
    // -----------------------------------------------------------------

    /** The process-wide name directory (profiles, verifications, renames). */
    private val profileDirectory: me.lakko.app.profile.ProfileDirectory by lazy {
        me.lakko.app.profile.ProfileDirectory.get(applicationContext)
    }

    /**
     * ADR-0048 §2 — the per-group republish scheduler. Join / founding and a
     * profile change publish at once; a group gaining a member republishes
     * throttled and jittered, so one add does not produce a synchronised burst
     * from every member. Runs on its own process-lifetime scope: a publish
     * takes the per-group lock through [broadcastControlToGroup], so it is
     * always LAUNCHED, never awaited under a lock.
     */
    private val profileRepublish: me.lakko.app.profile.ProfileRepublishScheduler =
        me.lakko.app.profile.ProfileRepublishScheduler(
            scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
            publish = { groupIdHex -> publishOwnProfile(groupIdHex) },
        )

    /** This account's own profile name, or `null` when none is set. The call
     *  host puts it on the user's own tile (`docs/ux/calls.md` section 4.4). */
    public fun ownDisplayName(): String? {
        val accountId = keyPackageAccount.get()?.accountId ?: return null
        return me.lakko.app.profile.OwnProfileStore.load(applicationContext, accountId)?.displayName
    }

    /** This account's own profile in wire form for [groupIdHex], or `null`
     *  when none is stored yet (nothing to publish). */
    private fun ownProfileForWire(groupIdHex: String, group: Group): uniffi.lakko_ffi_uniffi.FfiProfile? {
        val accountId = keyPackageAccount.get()?.accountId ?: return null
        // ADR-0060 §3 — a one-to-one carries `contact` + `member` and the
        // username; a group carries `member` always, and `group` + the username
        // only where the setting lets its members find this person.
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        val grants = reach.profileGrants(groupIdHex, oneToOne = isOneToOne(group))
        // ADR-0048 v2 — the profile states this account's id, so a member who
        // is not a contact can still name this device's call tile (ADR-0021).
        return me.lakko.app.profile.OwnProfileStore.load(applicationContext, accountId)?.toFfi(
            accountId = accountId,
            grants = grants.grants,
            usernameCertificate = reach.usernameCertificate(),
            shareUsername = grants.shareUsername,
        )
    }

    /**
     * ADR-0048 §2 — publish this device's own profile into [groupIdHex] as an
     * MLS application message, over the same locked, persist-before-publish
     * send path the committer beacon and the push-routing secret use.
     *
     * Skipped (returns `false`) when no profile is stored, when the group is
     * unknown, and when this device's membership has ended — an evicted device
     * cannot seal into the group and must not try.
     */
    private suspend fun publishOwnProfile(groupIdHex: String): Boolean =
        publishOwnProfileOutcome(groupIdHex) == me.lakko.app.reach.ProfileResend.SENT

    /** [publishOwnProfile], telling "nothing to send here" from "could not
     *  be sent" (ADR-0066 3.5a rule 3 keeps only the second owed). */
    private suspend fun publishOwnProfileOutcome(groupIdHex: String): me.lakko.app.reach.ProfileResend {
        val nothing = me.lakko.app.reach.ProfileResend.NOTHING_TO_SEND
        val failed = me.lakko.app.reach.ProfileResend.FAILED
        val identity = canonicalIdentity.get() ?: return failed
        val record = groups[groupIdHex] ?: return nothing
        if (readIsActive { record.group.isActive() } == false) return nothing
        val profile = ownProfileForWire(groupIdHex, record.group) ?: return nothing
        val envelope = try {
            uniffi.lakko_ffi_uniffi.composeProfileEnvelope(profile)
        } catch (cause: Throwable) {
            // A stored profile the core refuses (it was validated when saved,
            // so this is not expected) is never sent in a weakened form.
            android.util.Log.w("ChatRepository", "ADR-0048 own profile refused by the composer", cause)
            return nothing
        }
        return try {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
            )
            me.lakko.app.reach.ProfileResend.SENT
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0048 profile not published", cause)
            failed
        }
    }

    /** ADR-0066 3.5a rule 3 — the conversations that already took the
     *  profile re-sent under an account key (`"<keyHex>:<groupHex>"`), so a
     *  retry in this process does not send there again. In memory: after a
     *  restart an owed re-send goes to every conversation once more, which a
     *  receiver treats as the same profile. */
    private val profileResentTo: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** One re-send at a time. */
    private val profileResendMutex = kotlinx.coroutines.sync.Mutex()

    /**
     * ADR-0066 section 3.5a rule 3 — after the account's reach keys moved to
     * the mailbox of a new account key (the rotation's reach re-key on the
     * rotating device, the adoption on a kept one), every one-to-one and
     * group this device is in holds grants for the OLD mailbox. Re-send the
     * profile, which carries the grants for the new one, through the profile
     * publisher.
     *
     * Owed in the reach document from the moment the keys moved
     * ([me.lakko.app.reach.ReachManager.profileResendOwed]) until every
     * conversation took the envelope. `true`: nothing is owed (any more).
     * `false`: still owed — the new mailbox does not hold the key set yet,
     * the chat layer is not ready, or a conversation could not be written;
     * the rotation keeps its step, and every session start retries.
     */
    public suspend fun resendProfileAfterKeyChange(): Boolean {
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        if (!reach.profileResendOwed()) return true
        return profileResendMutex.withLock {
            if (!reach.profileResendOwed()) return@withLock true
            if (!mutableState.value.bootstrapped) return@withLock false
            // The grants must open the new mailbox before anyone is handed
            // them.
            if (!reach.sync()) return@withLock false
            val keyHex = reach.ownAccountKey()?.let { me.lakko.app.reach.reachHex(it) } ?: return@withLock false
            val targets = synchronized(groups) { groups.keys.toList() }.filter { "$keyHex:$it" !in profileResentTo }
            val done = profileRepublish.publishAllAwaited(
                groupIdHexes = targets,
                onSent = { groupIdHex -> profileResentTo += "$keyHex:$groupIdHex" },
                attempt = { groupIdHex -> publishOwnProfileOutcome(groupIdHex) },
            )
            if (done) {
                reach.profileResendDone()
                profileResentTo.removeIf { it.startsWith("$keyHex:") }
            } else {
                android.util.Log.w("ChatRepository", "ADR-0066 3.5a: the profile re-send is still owed")
            }
            done
        }
    }

    /**
     * ADR-0048 §2 — the own profile changed: publish it into every group this
     * device is in. Best-effort and fire-and-forget.
     */
    public fun publishOwnProfileToAllGroups() {
        val targets = synchronized(groups) { groups.keys.toList() }
        profileRepublish.publishNowToAll(targets)
        // ADR-0060 §5 — the Everyone publication follows the name.
        refreshEveryoneBestEffort()
        // ADR-0060 §4 — and so does the introduction record.
        runIntroductionsBestEffort()
    }

    /**
     * ADR-0060 §5 — enter or leave Everyone on discovery. In Everyone: the
     * lookup key of every `nameSearchTerms(displayName)` term, and the public
     * record composed over the OPRF OUTPUTS of those terms plus the username
     * (only a username this account holds a certificate for); republished
     * whenever the name, the username or the certificate changes. Outside it:
     * `public: false`, no terms, no record. Skipped when the server already
     * holds it; with neither a name nor a certified username nothing is sent
     * (the server refuses Everyone without a record).
     */
    private fun refreshEveryoneBestEffort() {
        val account = keyPackageAccount.get() ?: return
        reachScope.launch {
            try {
                val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
                val setting = reach.currentVisibility() ?: return@launch
                val everyone = setting.mode == uniffi.lakko_ffi_uniffi.FfiVisibilityMode.EVERYONE
                val own = me.lakko.app.profile.OwnProfileStore.load(applicationContext, account.accountId)
                val name = own?.displayName?.takeIf { it.isNotBlank() }
                val certificate = own?.username?.let { reach.usernameCertificate() }
                val key = me.lakko.app.reach.everyonePublishKey(name, own?.username, certificate)
                val (published, publishedKey) = reach.publishedEveryone()
                when (me.lakko.app.reach.everyoneAction(everyone, published, publishedKey, key)) {
                    me.lakko.app.reach.EveryoneAction.NONE -> return@launch
                    me.lakko.app.reach.EveryoneAction.NOTE_WITHDRAWN -> {
                        // Never published: nothing to withdraw beyond the default.
                        reach.notePublishedEveryone(false, null)
                        return@launch
                    }
                    me.lakko.app.reach.EveryoneAction.PUT -> Unit
                }
                val outcome = keyPackageDiscoveryFactory().use { client ->
                    client.setSessionToken(account.sessionToken)
                    me.lakko.app.reach.publishEveryone(
                        wantPublic = everyone,
                        who = me.lakko.app.reach.EveryoneIdentity(
                            displayName = name,
                            username = own?.username,
                            certificate = certificate,
                            identityPk = reach.ownAccountKey(),
                            accountId = account.accountId,
                        ),
                        ops = me.lakko.app.reach.EveryoneOps(
                            nameSearchTerms = { uniffi.lakko_ffi_uniffi.nameSearchTerms(it) },
                            oprfNameTerm = { client.oprfNameTerm(it) },
                            oprfUsername = { client.oprfUsername(it) },
                            composePublicRecord = { payload, outputs, issuedAt ->
                                uniffi.lakko_ffi_uniffi.composePublicRecord(payload, outputs, issuedAt)
                            },
                            putVisibility = { public, lookupKeys, record ->
                                client.putVisibility(public, lookupKeys, record)
                            },
                        ),
                        nowMs = System.currentTimeMillis(),
                    )
                }
                when (outcome) {
                    me.lakko.app.reach.EveryoneOutcome.PUBLISHED -> reach.notePublishedEveryone(true, key)
                    me.lakko.app.reach.EveryoneOutcome.WITHDRAWN -> reach.notePublishedEveryone(false, null)
                    me.lakko.app.reach.EveryoneOutcome.NOTHING_TO_PUBLISH ->
                        android.util.Log.i(
                            "ChatRepository",
                            "ADR-0060 Everyone not published: no name, no certified username or no identity",
                        )
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0060 Everyone publication failed: ${cause.javaClass.simpleName}")
            }
        }
    }

    // -----------------------------------------------------------------
    // ADR-0060 §4 — contacts of contacts: offer and relay
    // -----------------------------------------------------------------

    /** ADR-0060 §4 — how often the introductions re-check without a
     *  trigger. Chosen (the web runtime's `TICK_MS`), not measured. */
    private val INTRODUCTIONS_TICK_MS: Long = 5L * 60 * 1000

    /** One run at a time; a trigger during a run asks for one more. */
    private val introductionMutex = kotlinx.coroutines.sync.Mutex()

    @Volatile
    private var introductionsAgain = false

    /** The periodic re-check (web `TICK_MS`): an offer that arrived, a new
     *  one-to-one. Started on bind, cancelled on clear. */
    @Volatile
    private var introductionTicker: kotlinx.coroutines.Job? = null

    /**
     * ADR-0060 §4 — publish what contacts of contacts find this account by:
     * rebuild this account's introduction record when 2.1 is on and an input
     * moved, offer it in every one-to-one conversation not sent this version
     * yet, and relay the records this account's contacts offered as one
     * bundle (`me.lakko.app.reach.publishIntroductions`). Idempotent against
     * the stored state; best-effort and fire-and-forget.
     */
    public fun runIntroductionsBestEffort() {
        val account = keyPackageAccount.get() ?: return
        reachScope.launch {
            if (!introductionMutex.tryLock()) {
                introductionsAgain = true
                return@launch
            }
            try {
                do {
                    introductionsAgain = false
                    runIntroductions(account)
                } while (introductionsAgain)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w(
                    "ChatRepository",
                    "ADR-0060 introductions step failed; retried at the next run: ${cause.javaClass.simpleName}",
                )
            } finally {
                introductionMutex.unlock()
            }
        }
    }

    private suspend fun runIntroductions(account: KeyPackageAccount) {
        val identity = canonicalIdentity.get() ?: return
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        val setting = reach.currentVisibility() ?: return
        val fof = uniffi.lakko_ffi_uniffi.allowsContactsOfContacts(setting)
        val own = me.lakko.app.profile.OwnProfileStore.load(applicationContext, account.accountId)
        val username = own?.username?.takeIf { it.isNotEmpty() }
        val oneToOnes = synchronized(groups) { groups.entries.toList() }
            .filter { (_, record) ->
                readIsActive { record.group.isActive() } != false && isOneToOne(record.group)
            }
            .map { it.key }
            .sorted()
        keyPackageDiscoveryFactory().use { client ->
            me.lakko.app.reach.publishIntroductions(
                initial = reach.introPublicationState(),
                inputs = me.lakko.app.reach.IntroPublicationInputs(
                    contactsOfContacts = fof,
                    fofVersion = reach.fofVersion(),
                    fofGrant = reach.grant(me.lakko.app.reach.ReachKind.FOF),
                    displayName = own?.displayName,
                    username = username,
                    certificate = username?.let { reach.usernameCertificate() },
                    identityPk = reach.ownAccountKey(),
                    accountId = account.accountId,
                    offeredRecordsHex = reach.offeredIntroductionRecordsHex(),
                    oneToOneGroups = oneToOnes,
                    nowMs = System.currentTimeMillis(),
                ),
                ops = me.lakko.app.reach.IntroPublicationOps(
                    nameSearchTerms = { uniffi.lakko_ffi_uniffi.nameSearchTerms(it) },
                    oprfNameTerm = { client.oprfNameTerm(it) },
                    oprfUsername = { client.oprfUsername(it) },
                    composeIntroduction = { payload, outputs, issuedAt ->
                        uniffi.lakko_ffi_uniffi.composeIntroduction(payload, outputs, issuedAt)
                    },
                    composeOffer = { uniffi.lakko_ffi_uniffi.composeIntroductionOffer(it) },
                    composeBundle = { records, updatedAt ->
                        uniffi.lakko_ffi_uniffi.composeIntroductionsBundle(records, updatedAt)
                    },
                    maxBundleRecords = uniffi.lakko_ffi_uniffi.maxBundleRecords().toInt(),
                    send = { groupHex, envelope ->
                        val record = groups[groupHex] ?: throw IllegalStateException("conversation gone")
                        broadcastControlToGroup(
                            identity = identity,
                            groupIdHex = groupHex,
                            group = record.group,
                            envelope = envelope,
                        )
                    },
                    save = { reach.saveIntroPublicationState(it) },
                ),
            )
        }
    }

    /** Start the periodic introductions re-check for this session. */
    private fun startIntroductionTicker() {
        introductionTicker?.cancel()
        introductionTicker = reachScope.launch {
            while (true) {
                kotlinx.coroutines.delay(INTRODUCTIONS_TICK_MS)
                runIntroductionsBestEffort()
            }
        }
    }

    // -----------------------------------------------------------------
    // ADR-0048 trigger 4 — the "introduce yourselves" request
    // -----------------------------------------------------------------

    /** When this device last asked each group; persisted, so a reinstall of
     *  the app asks again and a restart does not. */
    private val profileRequestStore: me.lakko.app.profile.ProfileRequestStore by lazy {
        me.lakko.app.profile.ProfileRequestPrefsStore(applicationContext)
    }

    /** The decision, with the core's rule behind it (`shouldRequestProfiles`
     *  over UniFFI, so the three clients agree on when to ask). */
    private val profileRequestPolicy: me.lakko.app.profile.ProfileRequestPolicy by lazy {
        me.lakko.app.profile.ProfileRequestPolicy(
            store = profileRequestStore,
            rule = { joinedJustNow, unnamed, last, now ->
                uniffi.lakko_ffi_uniffi.shouldRequestProfiles(joinedJustNow, unnamed, last, now)
            },
        )
    }

    /** Process-lifetime scope for the request publish: it takes the per-group
     *  lock through [broadcastControlToGroup], so it is always LAUNCHED, never
     *  awaited under a lock, and it must not die with the screen that opened. */
    private val profileRequestScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * ADR-0048 trigger 4 — a member asked this group to introduce itself:
     * answer with this device's own profile through the growth path (throttled
     * to one per group per 30 s, jittered), so N members answering one request
     * do not land N envelopes in the same second. A device holding no profile
     * publishes nothing, as it does on growth.
     *
     * Not answered: an unattributable request (no sender this device can name
     * as a member) and one from this same PERSON on another device — that
     * device already knows this account's name. Called from the poll's
     * provider critical section (the own-identity read touches the `Group`);
     * the answer itself is scheduled onto the republish scope.
     */
    private fun answerProfileRequestBestEffort(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
    ) {
        if (senderKey == null) return
        val own = try {
            ownIdentity(group)
        } catch (_: Throwable) {
            null
        }
        if (own != null && own == senderKey) return
        profileRepublish.onGrowth(groupIdHex)
    }

    /**
     * ADR-0048 trigger 4 — on opening [groupIdHex]: if some member still has no
     * name on this device, ask the group once (at most once a day per group,
     * the core's `shouldRequestProfiles`). Fire-and-forget; the request is an
     * MLS application message over the audited locked send path. Heals a group
     * joined before the request existed, and one whose growth re-sends never
     * arrived.
     */
    public fun requestProfilesIfNeeded(groupIdHex: String) {
        profileRequestScope.launch { requestProfilesBestEffort(groupIdHex, joinedJustNow = false) }
    }

    /**
     * The one request routine. On a join it asks unconditionally (a new
     * membership is a new audience); on an open it counts the members whose
     * resolved name is the placeholder and lets the core's rule decide.
     *
     * The store is stamped only AFTER the publish went out (the way the
     * republish scheduler records a publish only when it was sent), so a
     * request that fails on a join is asked again on the next open rather than
     * silenced for a day.
     */
    private suspend fun requestProfilesBestEffort(groupIdHex: String, joinedJustNow: Boolean) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        // An evicted device cannot seal into the group and must not try.
        if (readIsActive { record.group.isActive() } == false) return
        val unnamed = if (joinedJustNow) {
            0
        } else {
            // A torn roster read decides nothing: this device cannot tell who
            // is unnamed, and a request on a guess is a journal row for nothing.
            val rows = members(groupIdHex) ?: return
            val ownLeaf = ownLeafIndex(groupIdHex)
            val ownKey = rows.firstOrNull { it.leafIndex == ownLeaf }?.signaturePublicKey
            me.lakko.app.profile.countUnnamedMembers(
                memberKeys = rows.map { it.signaturePublicKey },
                ownKey = ownKey,
                isPlaceholder = { key -> profileDirectory.resolve(key).isPlaceholder },
            )
        }
        val stamp = profileRequestPolicy.decide(
            accountIdB64 = accountIdB64,
            groupIdHex = groupIdHex,
            joinedJustNow = joinedJustNow,
            unnamedMembers = unnamed,
        ) ?: return
        try {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = uniffi.lakko_ffi_uniffi.composeProfileRequest(),
            )
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0048 profile request not published", cause)
            return
        }
        profileRequestPolicy.recordSent(accountIdB64, groupIdHex, stamp)
    }

    /**
     * ADR-0048 §3 — a received profile, bound to the sender's signature key the
     * classifier resolved at receipt (C-68). Stored only when strictly newer;
     * its username claim is then verified off this path. Called from the
     * poll's provider critical section, so it only writes the store and
     * LAUNCHES the verification.
     */
    private fun receiveProfileBestEffort(
        senderKey: MemberIdentity?,
        profile: uniffi.lakko_ffi_uniffi.FfiProfile,
    ) {
        try {
            profileDirectory.onProfileReceived(
                signatureKeyB64 = senderKey,
                profile = me.lakko.app.profile.PeerProfile(
                    displayName = profile.displayName,
                    username = profile.username,
                    updatedAtMs = profile.updatedAtMs,
                    // ADR-0048 v2 — the id the call roster derives the
                    // ADR-0021 peer tag from; kept per person key.
                    accountIdHex = me.lakko.app.profile.accountIdHexOrNull(profile.accountId),
                    // ADR-0060 §5 — the username shows only on its certificate.
                    usernameCertificateHex = profile.usernameCertificate?.let { me.lakko.app.reach.reachHex(it) },
                ),
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0048 profile not stored", cause)
        }
    }

    /**
     * ADR-0060 §3 / client contract, write paths — keep the reach grants a
     * profile hands out, ONLY where they may come from: `contact` from the
     * other participant of a one-to-one, `group` / `member` from a member of
     * this group, keyed on the sender's account key resolved at receipt
     * (C-68). Runs inside the poll's locked apply region (the roster read is
     * the epoch the profile was sealed at).
     */
    private fun receiveProfileGrantsBestEffort(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
        profile: uniffi.lakko_ffi_uniffi.FfiProfile,
    ) {
        if (profile.grants.isEmpty()) return
        try {
            val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
            val selfHex = reach.ownAccountKey()?.let { me.lakko.app.reach.reachHex(it) } ?: return
            val senderBytes = senderKey?.let { java.util.Base64.getDecoder().decode(it) } ?: return
            // ADR-0066 3.5a rule 2 — the roster names the keys the leaves are
            // bound to, which after a rotation are proven predecessors (this
            // account's own included). The acceptance rule is about PEOPLE,
            // so every key is mapped to its account's current key first.
            val senderHex = reach.mailboxAccountKeyHex(me.lakko.app.reach.reachHex(senderBytes))
            val people = personKeysOf(group.members())
            val oneToOne = people.size == 2
            val peopleHex = people.map {
                reach.mailboxAccountKeyHex(me.lakko.app.reach.reachHex(java.util.Base64.getDecoder().decode(it)))
            }
            val other = if (oneToOne) peopleHex.firstOrNull { it != selfHex } else null
            val isMember = senderHex in peopleHex
            for (grant in profile.grants) {
                val kind = grant.`class`.toKind()
                val venue = if (oneToOne) {
                    me.lakko.app.reach.GrantVenue.ONE_TO_ONE
                } else {
                    me.lakko.app.reach.GrantVenue.GROUP
                }
                if (!me.lakko.app.reach.acceptReceivedGrant(
                        kind = kind,
                        venue = venue,
                        senderKeyHex = senderHex,
                        selfKeyHex = selfHex,
                        otherParticipantKeyHex = other,
                        senderIsMember = isMember,
                    )
                ) {
                    continue
                }
                reach.keepHeld(senderBytes, grant, scopeHex = groupIdHex)
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0060 profile grants not kept: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * ADR-0060 — a scoped-discovery service envelope reached this group. A
     * committer delegation is kept per (sender resolved at receipt, added
     * account); an introduction offer / bundle is handed to the introductions
     * store; a Visibility or ReachState does not belong in a conversation and
     * is dropped. (The ADR-0057 registrar seed is not a scoped kind: it has its
     * own event and the owner gate in [receiveRegistrarSeed].)
     */
    private fun receiveScopedEnvelopeBestEffort(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
        kind: me.lakko.app.reach.ScopedEnvelopeKind,
        bytes: ByteArray,
    ) {
        try {
            val senderBytes = senderKey?.let { java.util.Base64.getDecoder().decode(it) } ?: return
            when (kind) {
                me.lakko.app.reach.ScopedEnvelopeKind.COMMITTER_DELEGATION -> {
                    val isMember = personKeysOf(group.members()).contains(senderKey)
                    if (isMember) {
                        me.lakko.app.reach.ReachManager.get(applicationContext)
                            .keepDelegation(groupIdHex, senderBytes, bytes)
                    }
                }
                me.lakko.app.reach.ScopedEnvelopeKind.INTRODUCTION_OFFER,
                me.lakko.app.reach.ScopedEnvelopeKind.INTRODUCTIONS -> {
                    // Only from the other participant of a one-to-one.
                    val people = personKeysOf(group.members())
                    if (people.size == 2 && people.contains(senderKey) && senderKey != ownIdentity(group)) {
                        introductionSink?.invoke(senderBytes, kind, bytes)
                    }
                }
                me.lakko.app.reach.ScopedEnvelopeKind.VISIBILITY,
                me.lakko.app.reach.ScopedEnvelopeKind.REACH_STATE -> Unit
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0060 service envelope dropped: ${cause.javaClass.simpleName}")
        }
    }

    /** ADR-0060 §4 — where an introduction offer / bundle from a contact goes
     *  (the contacts-of-contacts store). `null` drops it. */
    @Volatile
    public var introductionSink: ((ByteArray, me.lakko.app.reach.ScopedEnvelopeKind, ByteArray) -> Unit)? = null

    /** ADR-0048 §2 — republish on growth when the roster gained a member. */
    private fun republishProfileIfGrown(
        groupIdHex: String,
        previousMemberKeys: List<String>?,
        currentMemberKeys: List<String>?,
        selfEvicted: Boolean,
    ) {
        if (selfEvicted) return
        if (me.lakko.app.profile.membershipGrew(previousMemberKeys, currentMemberKeys)) {
            profileRepublish.onGrowth(groupIdHex)
            // ADR-0051 §3 — the newcomer cannot read the earlier envelope, so
            // an owner / admin holding the name re-sends it (original stamp),
            // on the same throttled + jittered schedule.
            groupNameRepublish.onGrowth(groupIdHex)
            // ADR-0052 decision 3 — and the shared pins, verbatim with their
            // original stamps, on the same schedule.
            pinRepublish.onGrowth(groupIdHex)
            // ADR-0059 section 6 — and this person's shared channel list,
            // verbatim with its original stamp, where the switch is on.
            sharedChannels.groupGrew(groupIdHex)
        }
    }

    // -----------------------------------------------------------------
    // ADR-0059 section 6 — channels shared inside a group
    // -----------------------------------------------------------------

    /**
     * The "Share my channels" switches, the lists members shared, and their
     * send / receive / growth re-send. The send is the audited control-message
     * publish ([broadcastControlToGroup]); the receive is launched from the
     * poll, never applied under the group lock.
     */
    /**
     * docs/ux/feed.md state table, "refresh failed (network)" — the feed's
     * failure state, app-wide and per account: the session shell's receive
     * loop and channel post sync report into it, and so do the feed's pull
     * to refresh and Retry. Bound at [bootstrap], reset in [clear].
     */
    public val feedRefresh: me.lakko.app.feed.FeedRefreshStatus = me.lakko.app.feed.FeedRefreshStatus()

    public val sharedChannels: me.lakko.app.discover.SharedChannelsRepository by lazy {
        me.lakko.app.discover.SharedChannelsRepository(
            account = { currentAccountIdB64.get() },
            broadcast = { groupIdHex, envelope ->
                val identity = canonicalIdentity.get() ?: throw IllegalStateException("Sign in again.")
                val record = groups[groupIdHex] ?: throw IllegalStateException("You are no longer in this group.")
                withContext(mlsDispatcher) {
                    broadcastControlToGroup(
                        identity = identity,
                        groupIdHex = groupIdHex,
                        group = record.group,
                        envelope = envelope,
                    )
                }
            },
            isLive = { groupIdHex ->
                val record = groups[groupIdHex]
                record != null && readIsActive { record.group.isActive() } != false
            },
            scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
            load = { me.lakko.app.discover.DiscoverStore.loadShared(applicationContext, it) },
            save = { acct, row -> me.lakko.app.discover.DiscoverStore.saveShared(applicationContext, acct, row) },
        )
    }

    // -----------------------------------------------------------------
    // ADR-0052 decision 3 — shared and personal pins
    // -----------------------------------------------------------------

    /** The growth re-send scheduler for the shared pins (the group-name shape). */
    private val pinRepublish: me.lakko.app.profile.ProfileRepublishScheduler =
        me.lakko.app.profile.ProfileRepublishScheduler(
            scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
            publish = { groupIdHex -> republishPinsIfHolder(groupIdHex) },
        )

    /** The conversation's shared pin map as this device holds it (unpins included). */
    public fun sharedPins(groupIdHex: String): Map<Long, StoredPin> =
        PinStore.loadShared(applicationContext, currentAccountIdB64.get(), groupIdHex)

    /** This device's personal pins in the conversation, `messageId -> pinnedAtMs`. */
    public fun personalPins(groupIdHex: String): Map<Long, ULong> =
        PinStore.loadPersonal(applicationContext, currentAccountIdB64.get(), groupIdHex)

    /**
     * A one-to-one by the kind its creator fixed in the group (ADR-0062) — a
     * two-person GROUP is not one, so its pins follow the role table
     * (ADR-0052 standing for both participants applies to a one-to-one only).
     * An unreadable kind counts as a group ([conversationKindOf]).
     */
    private fun isOneToOne(group: Group): Boolean =
        conversationKindOf(runCatching { group.groupKind() }.getOrNull()) == ConversationKind.Chat

    /**
     * Whether this device may set or clear a shared pin in [groupIdHex]: the
     * local half of the receive gate ([mayPinForEveryone]). Fails closed when
     * the role cannot be read in a role-table group.
     */
    public fun mayPinForEveryone(groupIdHex: String): Boolean {
        val identity = canonicalIdentity.get() ?: return false
        val record = groups[groupIdHex] ?: return false
        val oneToOne = isOneToOne(record.group)
        val hasTable = try {
            record.group.groupRoles() != null
        } catch (_: Throwable) {
            true // unreadable: treat as a table this device cannot see through
        }
        val role = try {
            record.group.ownRole(identity)
        } catch (_: Throwable) {
            null
        }
        return mayPinForEveryone(ownRole = role, hasRoleTable = hasTable, isOneToOne = oneToOne)
    }

    /**
     * ADR-0052 decision 3 — the receive gate. Called from the poll's locked
     * apply region (per-group lock + provider lock held), so the standing
     * read, the stored-row read, the verdict and the write are one serialized
     * read-decide-write against CURRENT state. Never touches the timeline.
     * An unpin is stored, not deleted.
     */
    private fun receivePin(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
        incoming: uniffi.lakko_ffi_uniffi.FfiPinOp,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val senderBytes = senderKey?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        val shared = PinStore.loadShared(applicationContext, accountIdB64, groupIdHex)
        val verdict = pinVerdict(
            senderSignatureKey = senderBytes,
            owner = roleTableOwner(group),
            isOneToOne = isOneToOne(group),
            incomingUpdatedAtMs = incoming.updatedAtMs,
            stored = shared[incoming.messageId],
            isNewer = { a, b -> uniffi.lakko_ffi_uniffi.pinIsNewer(a, b) },
        )
        if (!verdict.accepts) {
            android.util.Log.i("ChatRepository", "ADR-0052 pin refused: $verdict")
            return
        }
        try {
            val next = LinkedHashMap(shared)
            next[incoming.messageId] = StoredPin(pinned = incoming.pinned, updatedAtMs = incoming.updatedAtMs)
            PinStore.saveShared(applicationContext, accountIdB64, groupIdHex, next)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0052 pin not stored", cause)
        }
    }

    /** Pin or unpin [messageId] for this device only. Nothing is sent. */
    public fun setPersonalPin(groupIdHex: String, messageId: Long, pinned: Boolean) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val next = LinkedHashMap(PinStore.loadPersonal(applicationContext, accountIdB64, groupIdHex))
        if (pinned) next[messageId] = System.currentTimeMillis().toULong() else next.remove(messageId)
        PinStore.savePersonal(applicationContext, accountIdB64, groupIdHex, next)
    }

    /**
     * Pin or unpin [messageId] for everyone: compose the envelope with this
     * device's clock, publish it into the group, then apply it locally the
     * way a receiver would (the sender never receives its own message back).
     * Throws on a failed send; the caller shows the error.
     */
    public suspend fun setSharedPin(groupIdHex: String, messageId: Long, pinned: Boolean) {
        val identity = canonicalIdentity.get() ?: throw IllegalStateException("not bootstrapped")
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        val accountIdB64 = currentAccountIdB64.get() ?: throw IllegalStateException("no session")
        val stamp = System.currentTimeMillis().toULong()
        val op = uniffi.lakko_ffi_uniffi.FfiPinOp(messageId = messageId, pinned = pinned, updatedAtMs = stamp)
        val envelope = withContext(mlsDispatcher) { uniffi.lakko_ffi_uniffi.composePinEnvelope(op) }
        withContext(mlsDispatcher) {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
            )
        }
        // The local apply, under the same per-group lock a receiver holds, so
        // the stamp is compared against the current stored row (C-62).
        groupLock(groupIdHex).withLock {
            val shared = PinStore.loadShared(applicationContext, accountIdB64, groupIdHex)
            val stored = shared[messageId]
            if (stored == null || uniffi.lakko_ffi_uniffi.pinIsNewer(stamp, stored.updatedAtMs)) {
                val next = LinkedHashMap(shared)
                next[messageId] = StoredPin(pinned = pinned, updatedAtMs = stamp)
                PinStore.saveShared(applicationContext, accountIdB64, groupIdHex, next)
            }
        }
    }

    /**
     * ADR-0052 decision 3 — the growth re-send: a device with the standing to
     * set shared pins re-sends the current PINNED records verbatim (original
     * stamps, never a fresh clock read, so a re-send can never clobber a later
     * action). Unpins are not re-sent; a newcomer holds nothing to undo.
     */
    private suspend fun republishPinsIfHolder(groupIdHex: String): Boolean {
        val identity = canonicalIdentity.get() ?: return false
        val record = groups[groupIdHex] ?: return false
        if (readIsActive { record.group.isActive() } == false) return false
        val pinned = sharedPins(groupIdHex).filterValues { it.pinned }
        if (pinned.isEmpty()) return false
        if (!mayPinForEveryone(groupIdHex)) return false
        var sent = false
        for ((messageId, pin) in pinned) {
            val envelope = try {
                uniffi.lakko_ffi_uniffi.composePinEnvelope(
                    uniffi.lakko_ffi_uniffi.FfiPinOp(messageId = messageId, pinned = true, updatedAtMs = pin.updatedAtMs),
                )
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0052 stored pin refused by the composer", cause)
                continue
            }
            try {
                broadcastControlToGroup(
                    identity = identity,
                    groupIdHex = groupIdHex,
                    group = record.group,
                    envelope = envelope,
                )
                sent = true
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0052 pin not re-sent", cause)
            }
        }
        return sent
    }

    /**
     * Section 4.1 — a pinned message that is deleted leaves the bar and its
     * pin records go with it. Called after a batch that applied a delete, and
     * after this device's own delete; a store with no pins costs one read.
     */
    private fun dropPinsOfDeletedMessages(groupIdHex: String, record: GroupRecord) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            val shared = PinStore.loadShared(applicationContext, accountIdB64, groupIdHex)
            val personal = PinStore.loadPersonal(applicationContext, accountIdB64, groupIdHex)
            if (shared.isEmpty() && personal.isEmpty()) return
            val deleted = record.snapshot().asSequence().filter { it.deleted && it.messageId != 0L }.map { it.messageId }.toHashSet()
            if (deleted.isEmpty()) return
            if (shared.keys.any { it in deleted }) {
                PinStore.saveShared(applicationContext, accountIdB64, groupIdHex, shared.filterKeys { it !in deleted })
            }
            if (personal.keys.any { it in deleted }) {
                PinStore.savePersonal(applicationContext, accountIdB64, groupIdHex, personal.filterKeys { it !in deleted })
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0052 pins of deleted messages not dropped", cause)
        }
    }

    // -----------------------------------------------------------------
    // ADR-0051 — the shared group name
    // -----------------------------------------------------------------

    /**
     * ADR-0051 §3 — the growth re-send scheduler for the group name. The same
     * class as the profile republish: one per-group throttle + jitter, a
     * publish that is only ever launched, never awaited under a lock.
     */
    private val groupNameRepublish: me.lakko.app.profile.ProfileRepublishScheduler =
        me.lakko.app.profile.ProfileRepublishScheduler(
            scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
            publish = { groupIdHex -> republishGroupNameIfHolder(groupIdHex) },
        )

    /** The shared name this device holds for [groupIdHex], or `null`. */
    public fun groupName(groupIdHex: String): StoredGroupName? =
        GroupNameStore.load(applicationContext, currentAccountIdB64.get(), groupIdHex)

    /**
     * ADR-0051 §2 — the receive gate. Called from the poll's locked apply
     * region (per-group lock + provider lock held), so the role-table read,
     * the stored-row read, the verdict and the write are one serialized
     * read-decide-write against CURRENT state. Never touches the timeline.
     */
    private fun receiveGroupName(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
        incoming: uniffi.lakko_ffi_uniffi.FfiGroupName,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        // ADR-0062 section 7 — a one-to-one has no group name; applying one
        // would retitle the person. Dropped on receipt, read from the group's
        // own kind (the same answer on every member's device).
        val kind = try {
            group.groupKind()
        } catch (_: Throwable) {
            null
        }
        if (!groupNameAppliesTo(kind)) {
            android.util.Log.i("ChatRepository", "ADR-0062 group name dropped in a one-to-one")
            return
        }
        val owner = roleTableOwner(group)
        val senderBytes = senderKey?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        val stored = GroupNameStore.load(applicationContext, accountIdB64, groupIdHex)
        val candidate = StoredGroupName(name = incoming.name, updatedAtMs = incoming.updatedAtMs)
        val verdict = groupNameVerdict(
            senderSignatureKey = senderBytes,
            owner = owner,
            incoming = candidate,
            stored = stored,
            isNewer = { a, b -> uniffi.lakko_ffi_uniffi.groupNameIsNewer(a, b) },
        )
        if (!verdict.accepts) {
            android.util.Log.i("ChatRepository", "ADR-0051 group name refused: $verdict")
            return
        }
        if (verdict == GroupNameVerdict.ACCEPT_IDENTICAL) return
        try {
            GroupNameStore.save(applicationContext, accountIdB64, groupIdHex, candidate)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0051 group name not stored", cause)
        }
    }

    /**
     * ADR-0051 §3 — set (or, with a blank [name], clear) the group's shared
     * name: compose the envelope, publish it into the group, then persist the
     * stamped row. The local title changes only once the send succeeded, so a
     * failed rename never shows a name nobody else has (UX spec section 4).
     * Throws on a refused name or a failed send; the caller shows the error.
     *
     * With [storeFirst] the row is written before the send — the New group
     * path, where the creator's own name must survive even if the first send
     * fails (the growth re-send carries it later).
     */
    public suspend fun setGroupName(groupIdHex: String, name: String, storeFirst: Boolean = false) {
        val identity = canonicalIdentity.get() ?: throw IllegalStateException("not bootstrapped")
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        val accountIdB64 = currentAccountIdB64.get() ?: throw IllegalStateException("no session")
        val stamp = System.currentTimeMillis().toULong()
        // The composer trims and validates; a blank name composes as "clear".
        val envelope = withContext(mlsDispatcher) {
            uniffi.lakko_ffi_uniffi.composeGroupNameEnvelope(
                uniffi.lakko_ffi_uniffi.FfiGroupName(name = name, updatedAtMs = stamp),
            )
        }
        val row = StoredGroupName(name = name.trim(), updatedAtMs = stamp)
        if (storeFirst) GroupNameStore.save(applicationContext, accountIdB64, groupIdHex, row)
        withContext(mlsDispatcher) {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
            )
        }
        if (!storeFirst) GroupNameStore.save(applicationContext, accountIdB64, groupIdHex, row)
        publishThreadList()
    }

    /**
     * ADR-0051 §3 — the growth re-send: an owner / admin holding a non-empty
     * name re-sends it with the ORIGINAL stamp (never a fresh clock read, so
     * it can never clobber a later rename). A plain member in a role-table
     * group holds no authority and sends nothing; a member of a legacy group
     * with no role table may re-send, since the gate accepts it there.
     */
    private suspend fun republishGroupNameIfHolder(groupIdHex: String): Boolean {
        val identity = canonicalIdentity.get() ?: return false
        val record = groups[groupIdHex] ?: return false
        if (readIsActive { record.group.isActive() } == false) return false
        val stored = groupName(groupIdHex) ?: return false
        if (stored.name.isBlank()) return false
        val mayResend = try {
            when (record.group.ownRole(identity)) {
                Role.OWNER, Role.ADMIN -> true
                Role.MEMBER -> false
                null -> record.group.groupRoles() == null
            }
        } catch (_: Throwable) {
            false
        }
        if (!mayResend) return false
        val envelope = try {
            uniffi.lakko_ffi_uniffi.composeGroupNameEnvelope(
                uniffi.lakko_ffi_uniffi.FfiGroupName(name = stored.name, updatedAtMs = stored.updatedAtMs),
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0051 stored group name refused by the composer", cause)
            return false
        }
        return try {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
            )
            true
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0051 group name not re-sent", cause)
            false
        }
    }

    // -----------------------------------------------------------------
    // ADR-0065 phase 2 / UX section 16 — a contact reset their account keys
    // -----------------------------------------------------------------

    /** The per-contact reset records, read directly (the contacts repository
     *  writes them; this side only reads). */
    private val keyStateStore: me.lakko.app.contacts.ContactKeyStateStore =
        me.lakko.app.contacts.SharedPrefsContactKeyStateStore(applicationContext)

    /**
     * UX 16.1 point 2 — the identity check the scheduled device-list arm runs
     * per contact, through the contacts repository's fail-closed gate (which is
     * what records a logged reset). Set by the contacts repository at
     * construction; `null` runs no check (tests).
     */
    @Volatile
    public var contactIdentityCheck: (suspend (accountId: ByteArray) -> Unit)? = null

    /** The closed states read so far, per group hex; a missing entry is read
     *  from the store on first use. */
    private val closedStates = java.util.concurrent.ConcurrentHashMap<String, java.util.Optional<ConversationClosed>>()

    /** UX 16.4 / 16.5 — the conversation's closed state on this device, or `null`. */
    public fun conversationClosed(groupIdHex: String): ConversationClosed? {
        val account = currentAccountIdB64.get() ?: return null
        val key = groupIdHex.lowercase()
        return closedStates.getOrPut(key) {
            java.util.Optional.ofNullable(ConversationClosedStore.load(applicationContext, account, key))
        }.orElse(null)
    }

    /**
     * UX 16.4 — is this conversation a closed one-to-one? Such a conversation
     * is inert: no fetch, no apply, no send, no push subscription
     * ([conversationInert]). Every journal-facing entry point asks this first.
     */
    internal fun closedOneToOne(groupIdHex: String): Boolean =
        conversationInert(conversationClosed(groupIdHex))

    private fun closedOneToOneError(): ChatError =
        ChatError(
            "conversation is closed after the peer's key reset",
            IllegalStateException("closed one-to-one"),
            userText = OneToOneClosedCopy.COMPOSER,
        )

    /**
     * UX 16.4 — "no push subscription kept": retire this device's own push
     * row for a closed one-to-one. Launched, best-effort and idempotent (the
     * teardown answers `true` once nothing is subscribed), so it is safe to
     * run at the moment of closing and again whenever the chat is touched.
     */
    private fun retirePushForClosedOneToOne(groupIdHex: String) {
        val key = groupIdHex.lowercase()
        // Claimed before the launch so concurrent polls start one teardown;
        // released on anything but a completed teardown, so it is retried.
        if (!closedPushRetired.add(key)) return
        pushScope.launch {
            var done = false
            try {
                done = retirePushSubscriptionAfterEviction(groupIdHex)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0065 closed one-to-one: push teardown failed", cause)
            } finally {
                if (!done) closedPushRetired.remove(key)
            }
        }
    }

    /** Closed one-to-ones whose push row this session has retired (or is retiring). */
    private val closedPushRetired: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    private fun writeClosed(groupIdHex: String, closed: ConversationClosed) {
        val account = currentAccountIdB64.get() ?: return
        val key = groupIdHex.lowercase()
        try {
            ConversationClosedStore.save(applicationContext, account, key, closed)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0065 closed state not stored", cause)
        }
        closedStates[key] = java.util.Optional.of(closed)
    }

    /**
     * UX 16.5 — the receive gate for a group-closed envelope. Called from the
     * poll's locked apply region: the role-table read, the stored-row read,
     * the verdict and the write are one serialized read-decide-write against
     * CURRENT state, the group-name gate's shape exactly. Accepted only from
     * an owner or admin resolved at receipt (C-68); a later stamp replaces an
     * earlier one (the second notice to land wins `Open the new group`).
     */
    private fun receiveGroupClosed(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
        incoming: uniffi.lakko_ffi_uniffi.FfiGroupClosed,
    ) {
        currentAccountIdB64.get() ?: return
        val owner = roleTableOwner(group)
        val senderBytes = senderKey?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        val existing = conversationClosed(groupIdHex)?.takeIf { it.reason == ClosedReason.GROUP_CLOSED_BY_ADMIN }
        val stored = existing?.let { StoredGroupClosed(it.closedAtMs.toULong(), it.successorGroupIdHex) }
        val candidate = StoredGroupClosed(
            closedAtMs = incoming.closedAtMs,
            successorGroupIdHex = incoming.successorGroupId?.let { DsClient.pseudonymHex(it) },
        )
        val verdict = groupClosedVerdict(
            senderSignatureKey = senderBytes,
            owner = owner,
            incoming = candidate,
            stored = stored,
            isNewer = { a, b -> uniffi.lakko_ffi_uniffi.groupClosedIsNewer(a, b) },
        )
        if (!verdict.accepts) {
            android.util.Log.i("ChatRepository", "ADR-0065 group-closed notice refused: $verdict")
            return
        }
        if (verdict == GroupClosedVerdict.ACCEPT_IDENTICAL) return
        writeClosed(
            groupIdHex,
            ConversationClosed(
                reason = ClosedReason.GROUP_CLOSED_BY_ADMIN,
                closedAtMs = incoming.closedAtMs.toLong(),
                successorGroupIdHex = candidate.successorGroupIdHex,
                closerKeyB64 = senderKey,
            ),
        )
    }

    /**
     * UX 16.5 — `Create and close the old group`, the closing half: post the
     * group-closed envelope naming [successorGroupIdHex] into the OLD group
     * and record the closed state locally with the same stamp. The sender is
     * an admin (the receive gate on every peer re-checks that). Throws on a
     * failed send; the sheet keeps the new group and says so.
     */
    public suspend fun closeGroupNamingSuccessor(oldGroupIdHex: String, successorGroupIdHex: String) {
        val identity = canonicalIdentity.get() ?: throw IllegalStateException("not bootstrapped")
        val record = groups[oldGroupIdHex] ?: throw IllegalStateException("unknown group")
        val stamp = System.currentTimeMillis()
        val successor = hexToBytes(successorGroupIdHex)
        val envelope = withContext(mlsDispatcher) {
            uniffi.lakko_ffi_uniffi.composeGroupClosedEnvelope(stamp.toULong(), successor)
        }
        withContext(mlsDispatcher) {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = oldGroupIdHex,
                group = record.group,
                envelope = envelope,
            )
        }
        writeClosed(
            oldGroupIdHex,
            ConversationClosed(
                reason = ClosedReason.GROUP_CLOSED_BY_ADMIN,
                closedAtMs = stamp,
                successorGroupIdHex = successorGroupIdHex.lowercase(),
                closerKeyB64 = ownPersonKeyB64(),
            ),
        )
        // The notice is on the journal: the successor is no longer owed one.
        currentAccountIdB64.get()?.let { account ->
            try {
                ConversationClosedStore.clearPendingSuccessor(applicationContext, account, oldGroupIdHex)
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0065 recreate: pending successor not cleared", cause)
            }
        }
        publishThreadList()
    }

    /**
     * UX 16.5 — what `Create and close the old group` does for [oldGroupIdHex]
     * now: found a group, or only re-post the closed notice for the one an
     * earlier press founded ([recreateStep]).
     */
    public fun recreateStepFor(oldGroupIdHex: String): RecreateStep =
        recreateStep(
            pendingSuccessorHex = ConversationClosedStore.pendingSuccessor(
                applicationContext, currentAccountIdB64.get(), oldGroupIdHex,
            ),
            joinedGroupIdsHex = joinedGroupIdsHex(),
        )

    /**
     * UX 16.5 — record that [successorGroupIdHex] was founded to replace
     * [oldGroupIdHex], BEFORE its closed notice is attempted. Throws when the
     * record cannot be written: the caller must not go on to a step whose
     * failure would then be unrecoverable without a second group.
     */
    public fun rememberRecreateSuccessor(oldGroupIdHex: String, successorGroupIdHex: String) {
        val account = currentAccountIdB64.get() ?: throw IllegalStateException("not bootstrapped")
        ConversationClosedStore.savePendingSuccessor(applicationContext, account, oldGroupIdHex, successorGroupIdHex)
    }

    /**
     * 16.1 - 16.5 — a contact was flagged `reset-unverified`: run the three
     * branches over every conversation the old key is in. Launched; each group
     * is independent and a failure in one is logged, not propagated.
     */
    public fun onContactKeysReset(record: me.lakko.app.contacts.ResetRecord) {
        pushScope.launch {
            try {
                withContext(mlsDispatcher) { applyResetBranches(record) }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0065 reset branches failed", cause)
            }
        }
    }

    /** 16.2 — the contact was verified again: the list re-renders (the bubble
     *  warnings and the pending annotations read the records live). */
    public fun onContactReverified(@Suppress("UNUSED_PARAMETER") record: me.lakko.app.contacts.ResetRecord) {
        publishThreadList()
    }

    /** The per-session memory of evictions already proposed (group hex -> previous keys). */
    private val evictionsProposedThisSession = java.util.concurrent.ConcurrentHashMap<String, MutableSet<String>>()

    private suspend fun applyResetBranches(record: me.lakko.app.contacts.ResetRecord) {
        MainThreadGuard.assertOffMain("applyResetBranches")
        val groupIds = synchronized(groups) { groups.keys.toList() }
        for (groupIdHex in groupIds) {
            try {
                applyResetBranch(groupIdHex, record, onOpen = false)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0065 reset branch failed for $groupIdHex", cause)
            }
        }
        publishThreadList()
    }

    /**
     * 16.3 — on open of a chat: propose the eviction again for any flagged
     * contact whose old devices are still in the group and whose eviction has
     * not applied (the re-request discipline). Launched; best-effort.
     */
    public fun reproposeEvictionsOnOpen(groupIdHex: String) {
        val account = currentAccountIdB64.get() ?: return
        val records = keyStateStore.loadAll(account)
        if (records.isEmpty()) return
        pushScope.launch {
            for (record in records) {
                try {
                    withContext(mlsDispatcher) { applyResetBranch(groupIdHex, record, onOpen = true) }
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0065 re-proposal failed for $groupIdHex", cause)
                }
            }
        }
    }

    /** The branch a conversation falls in for a reset contact, as the UI reads it; `null` when the old key is not in it. */
    public fun resetBranchOf(groupIdHex: String, record: me.lakko.app.contacts.ResetRecord): KeysResetBranch? {
        val identity = canonicalIdentity.get() ?: return null
        val group = groups[groupIdHex]?.group ?: return null
        val roster = try {
            rosterOf(group)
        } catch (_: Throwable) {
            return null
        }
        val accountKeys = roster.map { outboxB64(personKeyOf(it)) }
        if (record.previousKeyB64 !in accountKeys && record.pendingKeyB64 !in accountKeys) return null
        val table = roleTableOrNull(group)
        val ownerIsPrevious = table?.owner?.let { outboxB64(it) } == record.previousKeyB64
        val role = try {
            group.ownRole(identity)
        } catch (_: Throwable) {
            null
        }
        return keysResetBranch(
            kind = groupKind(groupIdHex),
            hasRoleTable = table != null,
            ownerIsPrevious = ownerIsPrevious,
            ownRole = role,
        )
    }

    private suspend fun applyResetBranch(groupIdHex: String, record: me.lakko.app.contacts.ResetRecord, onOpen: Boolean) {
        val account = currentAccountIdB64.get() ?: return
        val identity = canonicalIdentity.get() ?: return
        val group = groups[groupIdHex]?.group ?: return
        if (readIsActive { group.isActive() } == false) return
        val roster = providerLock.withLock {
            try {
                rosterOf(group)
            } catch (_: Throwable) {
                null
            }
        } ?: return
        val previousLeaves = roster.filter { outboxB64(personKeyOf(it)) == record.previousKeyB64 }
        val branch = resetBranchOf(groupIdHex, record) ?: return
        when (branch) {
            KeysResetBranch.OneToOneClose -> {
                if (onOpen) return
                if (previousLeaves.isEmpty()) return
                if (conversationClosed(groupIdHex) != null) return
                writeClosed(
                    groupIdHex,
                    ConversationClosed(
                        reason = ClosedReason.PEER_KEYS_RESET,
                        closedAtMs = record.observedAtMs,
                        peerAccountIdB64 = record.accountIdB64,
                    ),
                )
                android.util.Log.i("ChatRepository", "ADR-0065 one-to-one closed after the peer's key reset")
                // UX 16.4 — no push subscription is kept for a closed chat.
                retirePushForClosedOneToOne(groupIdHex)
                // A call live in this chat can no longer publish its `End`:
                // it ends locally, now.
                try {
                    oneToOneClosedSink?.invoke(groupIdHex)
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0065 closed one-to-one: call not ended", cause)
                }
            }
            KeysResetBranch.EvictionProposal -> {
                val stored = EvictionStore.load(applicationContext, account, groupIdHex, record.previousKeyB64)
                if (previousLeaves.isEmpty()) {
                    if (stored != null && !stored.applied) {
                        EvictionStore.save(applicationContext, account, groupIdHex, stored.copy(applied = true))
                    }
                    return
                }
                val proposed = evictionsProposedThisSession.getOrPut(groupIdHex) { java.util.Collections.synchronizedSet(HashSet()) }
                // 16.3 — on open: a request this session already made that is
                // no longer in the pending set (dropped by a sweep that did not
                // carry it) is re-sent; one still pending is not duplicated.
                if (onOpen && record.previousKeyB64 in proposed) {
                    val previousLeafIndices = previousLeaves.map { it.leafIndex }.toSet()
                    val stillPending = pendingMembershipRequests(groupIdHex).any {
                        it.kind == PendingRequestKind.REMOVE && it.targetLeaf != null && it.targetLeaf in previousLeafIndices
                    }
                    if (!stillPending) proposed -= record.previousKeyB64
                }
                if (!shouldProposeEviction(stored, proposedThisSession = record.previousKeyB64 in proposed)) return
                proposed += record.previousKeyB64
                // One person: `removeMemberFromGroup` fans out to every leaf of
                // the account the named leaf belongs to (ADR-0050), through the
                // proposal path of a role-table group (ADR-0045).
                val leaf = previousLeaves.first().leafIndex
                try {
                    removeMemberFromGroup(groupIdHex, leaf)
                    EvictionStore.save(
                        applicationContext, account, groupIdHex,
                        EvictionRecord(record.previousKeyB64, proposedAtMs = System.currentTimeMillis(), applied = false),
                    )
                    android.util.Log.i("ChatRepository", "ADR-0065 eviction proposed in $groupIdHex")
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    // A refusal for a full queue is re-sent when the queue
                    // drains: the next open runs this again.
                    proposed -= record.previousKeyB64
                    android.util.Log.w("ChatRepository", "ADR-0065 eviction not proposed", cause)
                }
                sweepIfCommitter(groupIdHex)
            }
            KeysResetBranch.FlagOnly, KeysResetBranch.OwnedGroupNotice, KeysResetBranch.NoRoleTableFlag -> Unit
        }
    }

    /**
     * 16.3 — after a batch that changed the roster: for every flagged (or
     * verified) contact whose eviction was proposed here and whose previous
     * key no longer resolves to any leaf, mark it applied and append the
     * reset's own system row on this device, which knows the reset.
     */
    private fun noteEvictionsApplied(record: GroupRecord, groupIdHex: String, accountKeysBefore: List<String>?) {
        val account = currentAccountIdB64.get() ?: return
        val accountKeysAfter = memberAccountKeysOrNull(record.group)
        val directory = me.lakko.app.profile.ProfileDirectory.get(applicationContext)
        // The device proposed it: the bookkeeping row flips to applied.
        val evictions = EvictionStore.loadForGroup(applicationContext, account, groupIdHex)
        for (eviction in evictions) {
            if (eviction.applied) continue
            if (!evictionApplied(accountKeysAfter, eviction.previousKeyB64)) continue
            EvictionStore.save(applicationContext, account, groupIdHex, eviction.copy(applied = true))
        }
        // Every device that KNOWS the reset (the contact is flagged there, or
        // was until verified) draws the reset's own system row when the old
        // devices leave; a device that does not keeps the plain members row.
        if (accountKeysBefore == null) return
        for (reset in keyStateStore.loadAll(account)) {
            if (reset.previousKeyB64 !in accountKeysBefore) continue
            if (!evictionApplied(accountKeysAfter, reset.previousKeyB64)) continue
            val name = directory.resolve(reset.previousKeyB64).primary
            record.appendLocalSystemRow(evictionLandedRow(knowsReset = true, name = name))
        }
    }

    /** The roster's ACCOUNT keys (standard base64), or `null` on a torn read.
     *  Must be called on the [providerLock] critical path. */
    private fun memberAccountKeysOrNull(group: Group): List<String>? =
        try {
            rosterOf(group).map { outboxB64(personKeyOf(it)) }
        } catch (_: Throwable) {
            null
        }

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
        // UX 16.4 — no service envelope (profile, pin, routing secret, beacon)
        // is sent into a closed one-to-one either.
        if (closedOneToOne(groupIdHex)) return
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
        // UX 16.4 — a closed one-to-one keeps no push subscription, so the
        // bootstrap sweep must not put one back.
        if (closedOneToOne(groupIdHex)) return
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
            // ADR-0050 — authority is the PERSON's: the role table names
            // account keys, so the sender leaf resolves to its account key.
            group.members().firstOrNull { it.leafIndex == leafIndex }?.let { personKeyOf(it) }
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
        // ADR-0060 §3 — a group that shrank rotates this account's own
        // `group` and `member` keys for it (the evicted member holds the old
        // ones), then republishes the profile carrying the new grants. This
        // runs after the Remove commit was applied, so the republished profile
        // is sealed at an epoch the evicted member cannot open (the C-61
        // shape). Every member does this for its own keys, whatever its role.
        if (me.lakko.app.reach.reachRotationOnShrink(previousMemberKeys, currentMemberKeys, selfEvicted)) {
            reachScope.launch {
                try {
                    val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
                    reach.rotate(me.lakko.app.reach.ReachKind.MEMBER, groupIdHex)
                    reach.rotate(me.lakko.app.reach.ReachKind.GROUP, groupIdHex)
                    profileRepublish.publishNow(groupIdHex)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0060 shrink rotation failed: ${cause.javaClass.simpleName}")
                }
            }
        }
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
    /**
     * Returns what this call did — see [GroupPollOutcome]. Only
     * [GroupPollOutcome.APPLIED] fetched and applied a batch here; a loop must
     * stop or back off after [GroupPollOutcome.UNAVAILABLE], since that call
     * returned at once and the next would too.
     */
    /**
     * @param joinInFlight when another poll of this group is in flight, WAIT
     *   for it to finish (its apply included) before returning
     *   [GroupPollOutcome.SKIPPED_IN_FLIGHT], instead of returning at once. The
     *   chat screen's continuous loop passes `true`, which is what lets it call
     *   straight back in without a blind sleep. A one-shot refresh keeps the
     *   default and returns immediately.
     */
    public suspend fun pollGroup(
        groupIdHex: String,
        waitSecs: Int = 0,
        joinInFlight: Boolean = false,
    ): GroupPollOutcome = withContext(mlsDispatcher) {
        pollGroupOnMlsThread(groupIdHex, waitSecs, joinInFlight)
    }

    private suspend fun pollGroupOnMlsThread(
        groupIdHex: String,
        waitSecs: Int,
        joinInFlight: Boolean,
    ): GroupPollOutcome {
        // Not before the bootstrap has finished restoring + seeding: a poll
        // that ran between `canonicalIdentity` landing and the message-log
        // seed would fetch from cursor 0 only to have the batch dropped as
        // cursor-moved under the lock.
        if (!mutableState.value.bootstrapped) return GroupPollOutcome.UNAVAILABLE
        val identity = canonicalIdentity.get() ?: return GroupPollOutcome.UNAVAILABLE
        val record = groups[groupIdHex] ?: return GroupPollOutcome.UNAVAILABLE

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
        if (readIsActive { record.group.isActive() } == false) return GroupPollOutcome.UNAVAILABLE

        // ADR-0065 phase 2 / UX 16.4 — a closed one-to-one stops fetching its
        // journal altogether (the iOS shape; web suspends its receive loop).
        // The same GATE as above and for the same reason: every re-entry point
        // lands here. Whatever an old device still sends into the old journal
        // is therefore never fetched, so no op is applied, no call is offered
        // or rung, no typing or read state moves. The push row is retired
        // here too, so a chat closed while the teardown could not run (offline)
        // retries it the next time anything touches the chat.
        if (closedOneToOne(groupIdHex)) {
            retirePushForClosedOneToOne(groupIdHex)
            return GroupPollOutcome.UNAVAILABLE
        }

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
        // ADR-0057 — the epoch before this batch applied: an owner whose batch
        // advanced it (a member joined or a role changed under someone else's
        // commit) reconciles its registrar appointments at the tail. Plain
        // members no longer "heal" a registration (obligation 6).
        var epochBeforeBatch: ULong? = null
        val outcome = runSplitPoll(
            inFlight = record.pollInFlight,
            joinInFlight = joinInFlight,
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
            MainThreadGuard.assertOffMain("pollGroup apply")
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
            // ADR-0065 phase 2 / UX 16.3 — the ACCOUNT keys before the batch,
            // so an eviction landing in it is judged on the previous key.
            val accountKeysBeforePoll = memberAccountKeysOrNull(record.group)
            epochBeforeBatch = runCatching { record.group.epoch() }.getOrNull()
            // ADR-0057 S2 finding 1 — the owner before this batch, so a remote
            // commit that carried this account's ownership transfer is seen.
            val ownerBeforeBatch = roleTableOrNull(record.group)?.owner
            val result = chatClient.applyFetchedMessages(
                identity = identity,
                group = record.group,
                raw = raw,
                after = after,
                // The journal these rows came from. The own-echo filter is per
                // journal: without it an id published in another chat dropped
                // this chat's row of the same number, unread and unrecoverable.
                groupIdHex = groupIdHex,
            )
            record.consumeEvents(
                events = result.events,
                callSignalingSink = { messageId, payload, createdAt, senderLeaf, senderKey, senderPersonKey ->
                    routeIncomingCallSignaling(
                        groupIdHex, messageId, payload, createdAt, senderLeaf, senderKey, senderPersonKey,
                    )
                },
                profileSink = { senderKey, profile ->
                    receiveProfileBestEffort(senderKey, profile)
                    // ADR-0060 §3 — keep the grants the profile hands out,
                    // gated on who sent them and where (C-68).
                    receiveProfileGrantsBestEffort(record.group, groupIdHex, senderKey, profile)
                },
                scopedSink = { senderKey, kind, bytes ->
                    receiveScopedEnvelopeBestEffort(record.group, groupIdHex, senderKey, kind, bytes)
                },
                groupNameSink = { senderKey, name ->
                    // ADR-0051 — inside the poll's locked apply region, where
                    // the role table read and the store write are serialized
                    // per group (C-62 sub-invariant 5).
                    receiveGroupName(record.group, groupIdHex, senderKey, name)
                },
                groupClosedSink = { senderKey, closed ->
                    // ADR-0065 phase 2 / UX 16.5 — the same critical section
                    // as the group name: the role gate, the stored-row read,
                    // the stamp order and the write are one read-decide-write.
                    receiveGroupClosed(record.group, groupIdHex, senderKey, closed)
                },
                inert = closedOneToOne(groupIdHex),
                pinSink = { senderKey, op ->
                    // ADR-0052 decision 3 — same critical section as the group
                    // name: the standing read, the stored-row read, the verdict
                    // and the write are one serialized read-decide-write.
                    receivePin(record.group, groupIdHex, senderKey, op)
                },
                sharedChannelsSink = { senderKey, list ->
                    // ADR-0059 section 6 — LAUNCHED onto the shared-channels
                    // scope, never applied under this lock (see
                    // SharedChannelsRepository on the lock order).
                    sharedChannels.receive(groupIdHex, senderKey, list)
                },
                registrarSeedSink = { senderKey, envelope ->
                    // ADR-0057 — inside the locked apply region, so the owner
                    // is read off the role table as it stands at receipt.
                    receiveRegistrarSeed(record.group, groupIdHex, senderKey, envelope)
                },
                profileRequestSink = { senderKey ->
                    // ADR-0048 trigger 4 — inside the poll's provider critical
                    // section, where the own-identity read is safe; the answer
                    // itself is scheduled, never sent from here.
                    answerProfileRequestBestEffort(record.group, groupIdHex, senderKey)
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
            // ADR-0057 S2 finding 1 — the ownership moved away from this
            // account in this batch: hand the journal root over (scheduled).
            noteOwnerTransition(groupIdHex, ownerBeforeBatch, roleTableOrNull(record.group)?.owner)
            // ADR-0052 decision 3 — a delete that landed takes its message's
            // pins with it (section 4.1: no tombstone in the bar).
            if (result.events.any { it is IncomingChatEvent.Op && it.op == MessageOp.Delete }) {
                dropPinsOfDeletedMessages(groupIdHex, record)
            }
            // ADR-0024 — a receipt advanced the watermark: persist it off the lock.
            persistReadWatermarksIfReceipts(groupIdHex, record, result.events)
            // The chat list's unread badge: incoming chat messages applied
            // while this chat is not on screen (see [LocalUnreadCounter]).
            localUnread.onIncomingApplied(
                groupIdHex,
                result.events.count { it is IncomingChatEvent.Application },
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
            val membersAfterPoll = memberSignatureKeysOrNull(record.group)
            val evictedByBatch = readIsActive { record.group.isActive() } == false
            rotatePushRoutingIfShrankBestEffort(
                groupIdHex = groupIdHex,
                group = record.group,
                identity = identity,
                previousMemberKeys = membersBeforePoll,
                currentMemberKeys = membersAfterPoll,
                selfEvicted = evictedByBatch,
            )
            // ADR-0048 §2 — somebody else's commit added a member: the newcomer
            // cannot read earlier epochs, so republish this device's profile
            // (throttled + jittered per group). Same key-set diff as the shrink
            // above, so a remove-and-add in one batch is still growth.
            // ADR-0050 — remember the leaves that left, so the scheduled
            // re-admission never puts back a device an admin removed.
            currentAccountIdB64.get()?.let { account ->
                DeviceReadmission.recordRemoved(
                    applicationContext,
                    account,
                    groupIdHex,
                    membersBeforePoll,
                    membersAfterPoll,
                )
            }
            republishProfileIfGrown(
                groupIdHex = groupIdHex,
                previousMemberKeys = membersBeforePoll,
                currentMemberKeys = membersAfterPoll,
                selfEvicted = evictedByBatch,
            )
            // ADR-0065 phase 2 / UX 16.3 — did this batch carry the eviction of
            // a reset contact's old devices? Judged on the post-batch roster's
            // ACCOUNT keys against each flagged contact's previous key.
            if (membersBeforePoll != null && membersAfterPoll != null && membersBeforePoll != membersAfterPoll) {
                noteEvictionsApplied(record, groupIdHex, accountKeysBeforePoll)
            }
            publishThreadList()
        }
        // Nothing was fetched or applied by THIS call: the poll that was in
        // flight (or that moved the cursor) runs its own tail. Re-running the
        // sweep prelude and the re-admission check here would only burn the
        // provider lock and FFI reads on a result that cannot have changed.
        if (outcome != SplitPollOutcome.APPLIED) return outcome.toGroupPollOutcome()
        // ADR-0057 obligation 3 — outside both locks; launched, best effort,
        // and only when the batch moved the epoch (a no-op for a non-owner).
        val epochAfterBatch = providerLock.withLock { runCatching { record.group.epoch() }.getOrNull() }
        if (epochBeforeBatch != null && epochAfterBatch != null && epochAfterBatch > epochBeforeBatch!!) {
            reconcileAppointmentsBestEffort(groupIdHex)
        }
        // ADR-0057 S2 finding 1 — a root handover this account still owes
        // (the new owner's key did not resolve yet, or a send failed) is
        // retried from the poll tail, no sooner than its retry interval.
        scheduleRootHandover(groupIdHex)
        scheduleTakeover(groupIdHex)
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
        // ADR-0050 section 2.5a point 4 — a merged commit (a remote one this
        // batch applied, or the sweep just above) may have dropped a request
        // this device made. Re-request it from its intent. After the sweep, so
        // the epoch read includes what this device itself just merged; outside
        // both locks, because a re-request takes the per-group lock itself.
        try {
            reRequestSupersededAfterCommit(groupIdHex, epochBefore = epochBeforeBatch)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0050 superseded re-request pass failed", cause)
        }
        // ADR-0053 client contract point 7 — an owner / admin also reads what
        // non-members knocked, into the same classifier. Best effort.
        try {
            pollKnocksIfOwnerOrAdmin(groupIdHex)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0053 knock poll failed", cause)
        }
        // ADR-0050 section 2.4 — the scheduled re-admission check, at most once
        // per [DeviceReadmission.INTERVAL_MS] per chat. Best-effort: a failure
        // is not a poll failure.
        try {
            readmitIfDue(groupIdHex)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0050 re-admission check failed", cause)
        }
        return GroupPollOutcome.APPLIED
    }

    /**
     * ADR-0053 client contract point 7 — an owner or admin reads the group's
     * knock queue (`GET .../knocks`, signed, its own cursor) and feeds each
     * row through the core's knock gate (`Group.processKnock`), which admits
     * only a stranger's external-join proposal for this group and refuses
     * anything else before processing it. Never the ordinary receive path.
     *
     * Throttled per group ([KNOCK_POLL_MIN_INTERVAL_MS]); a plain member never
     * reads knocks — gated on the ROLE ([readsKnocks]), not on the committer
     * rank, which a plain member also holds while it can sweep a device
     * admission (ADR-0050 section 2.5a point 7). The cursor lives in memory: after a restart the retained
     * knocks (seven days) are re-read and fall to the quarantine's dedup or to
     * the epoch check.
     */
    private suspend fun pollKnocksIfOwnerOrAdmin(groupIdHex: String) {
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        val now = System.currentTimeMillis()
        if (now - record.lastKnockPollMs < KNOCK_POLL_MIN_INTERVAL_MS) return
        val ownerOrAdmin = providerLock.withLock {
            try {
                readsKnocks(record.group.ownRole(identity))
            } catch (_: Throwable) {
                false
            }
        }
        if (!ownerOrAdmin) return
        record.lastKnockPollMs = now
        val after = record.knockCursor
        val raw = dsClient.fetchKnocks(record.groupId, after = after, waitSecs = 0)
        if (raw.messages.isEmpty()) return
        groupLock(groupIdHex).withLock {
            providerLock.withLock {
                // Another pass moved the cursor while this one fetched.
                if (record.knockCursor != after) return@withLock
                // Every row through the core's knock gate and never through
                // the ordinary receive path: a knock is an unauthenticated
                // write, and a member could knock a real commit (see
                // `KnockGate.kt`). A refused row is dropped, the cursor moves.
                val drained = drainKnockRows(raw.messages, after) { bytes ->
                    record.group.processKnock(identity, bytes)
                }
                val events = drained.accepted.mapNotNull { (row, processed) ->
                    if (processed !is uniffi.lakko_ffi_uniffi.ProcessedMessage.ExternalJoinProposal) {
                        return@mapNotNull null
                    }
                    IncomingChatEvent.ExternalJoinRequest(
                        messageId = row.id,
                        joinerSignatureKey = processed.joinerSignatureKey,
                        retained = processed.retained,
                        createdAt = row.createdAt,
                    )
                }
                record.consumeEvents(events = events)
                // ADR-0060 §6 — a `/g/` joiner's attribution carries its reply
                // key: kept for the joiner's account (named by the retained
                // KeyPackage's device binding), so the sweep that admits it
                // can write the Welcome into its closed mailbox.
                keepJoinerReplyGrantsBestEffort(record, raw.messages)
                // The retained request set lives in the provider (ADR-0027).
                if (drained.accepted.isNotEmpty()) persistSnapshotBestEffort()
                record.knockCursor = drained.cursor
            }
        }
        publishThreadList()
    }

    // -----------------------------------------------------------------
    // ADR-0050 section 2.5a point 4 — re-requesting a superseded request.
    //
    // Every rule lives in `MembershipRetry.kt` (`supersededVerdict`,
    // `reRequestFailure`, `supersededCheckDue`), where the host suite runs it;
    // what is here feeds it live group state and re-issues what it decides.
    // -----------------------------------------------------------------

    /** The epoch the superseded check last ran at, per group. In memory: a
     *  fresh session waits for the next commit rather than treating its own
     *  start as one. */
    private val supersededCheckedEpoch = java.util.concurrent.ConcurrentHashMap<String, ULong>()

    /** Groups a superseded pass is running for — a second tail (two polls
     *  finishing close together) skips rather than re-sending twice. */
    private val supersededPassRunning: MutableSet<String> =
        java.util.concurrent.ConcurrentHashMap.newKeySet()

    /**
     * After a merged commit, decide each own recorded request: landed, still
     * pending, superseded (re-request it, silently), or stopped (list it under
     * `Not applied`).
     *
     * Runs only when the epoch moved since the last pass ([supersededCheckDue]),
     * so the trigger is an observed commit and never a timer. The group is read
     * in ONE provider region, so the roster and the pending set describe the
     * same epoch. Each re-request goes through the same propose function the
     * user path uses — same locks, same ADR-0040 / ADR-0057 steps — which also
     * records the intent again.
     */
    private suspend fun reRequestSupersededAfterCommit(groupIdHex: String, epochBefore: ULong?) {
        val identity = canonicalIdentity.get() ?: return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val record = groups[groupIdHex] ?: return
        if (!supersededPassRunning.add(groupIdHex)) return
        try {
            val read = providerLock.withLock {
                val epochNow = runCatching { record.group.epoch() }.getOrNull() ?: return
                if (!supersededCheckDue(supersededCheckedEpoch[groupIdHex], epochBefore, epochNow)) {
                    // Never above the live epoch: an ADR-0040 reset to the
                    // durable snapshot can move it back, and the next commit
                    // from there must still count.
                    supersededCheckedEpoch[groupIdHex] = epochNow
                    return
                }
                supersededCheckedEpoch[groupIdHex] = epochNow
                supersededSnapshotLocked(record)
            }
            val result = runSupersededPass(
                snapshot = read,
                store = pendingRequestObservations,
                notApplied = unappliedRequests,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                reRequest = { intent -> reRequestIntent(groupIdHex, identity, intent) },
                retryMaySucceed = ::membershipRefusalRetryMaySucceed,
            )
            val reRequested = result.reRequested
            if (result.notApplied > 0) {
                android.util.Log.i("ChatRepository", "ADR-0050 ${result.notApplied} own request(s) stopped: not applied")
            }
            if (reRequested > 0) {
                android.util.Log.i("ChatRepository", "ADR-0050 re-requested $reRequested superseded request(s)")
                // The same immediate sweep the user path runs after proposing.
                sweepIfCommitter(groupIdHex)
            }
        } finally {
            supersededPassRunning.remove(groupIdHex)
        }
    }

    /** The group as [supersededVerdict] reads it. Under [providerLock]. A failed
     *  read is `null` where the verdict treats `null` as "claims nothing". */
    private fun supersededSnapshotLocked(record: GroupRecord): RetryGroupSnapshot {
        val roster = try {
            record.group.members().map {
                RetryRosterMember(it.leafIndex, outboxB64(it.signaturePublicKey))
            }
        } catch (_: Throwable) {
            null
        }
        val pending = try {
            record.group.pendingProposals()
        } catch (_: Throwable) {
            null
        }
        val table = roleTableOrNull(record.group)?.let {
            RetryRoleTable(outboxB64(it.owner), it.admins.map(::outboxB64))
        }
        val retained = try {
            record.group.pendingExternalJoins().map(::outboxB64)
        } catch (_: Throwable) {
            null
        }
        return RetryGroupSnapshot(
            // A pending set that could not be read makes the whole read torn:
            // a missing proposal would otherwise look superseded.
            roster = if (pending == null) null else roster,
            pendingKeys = pending.orEmpty().map(::proposalKey),
            pendingCount = pending?.size ?: 0,
            roleTable = table,
            membershipEnded = readIsActive { record.group.isActive() } == false,
            retainedJoinKeys = retained,
        )
    }

    /**
     * Re-issue one superseded request, RE-DERIVED from its intent against the
     * current epoch. `false` when its subject is gone (nothing to re-request);
     * throws what the propose path throws.
     */
    private suspend fun reRequestIntent(
        groupIdHex: String,
        identity: Identity,
        intent: RetryableRequest,
    ): Boolean {
        when (intent) {
            is RetryableRequest.Add -> {
                // A fresh KeyPackage for the SAME device: the one first sent is
                // single-use and was consumed with the dropped proposal.
                val accountId = outboxUnb64(intent.subjectAccountIdB64)
                val subject = outboxUnb64(intent.subjectKeyB64)
                val listed = discovery.fetchKeyPackages(accountId) ?: return false
                val device = listed.firstOrNull { it.deviceId.contentEquals(subject) } ?: return false
                addKeyPackageToGroup(groupIdHex, identity, device.keyPackage, skipIfPresent = true, accountId = accountId)
                    ?: return false
            }
            is RetryableRequest.ApproveJoin ->
                // From the retained KeyPackage: the verdict only re-requests an
                // approve whose joiner is back in the quarantine.
                approveExternalJoin(groupIdHex, outboxUnb64(intent.subjectKeyB64))
            // The leaf is resolved from the live roster by signature key inside
            // `retireLeafFromGroup`; a leave is the own leaf's removal, and
            // every other own leaf carries its own intent.
            is RetryableRequest.Remove ->
                withContext(mlsDispatcher) {
                    retireLeafFromGroup(groupIdHex, identity, outboxUnb64(intent.subjectKeyB64))
                } ?: return false
            is RetryableRequest.Leave ->
                withContext(mlsDispatcher) {
                    retireLeafFromGroup(groupIdHex, identity, outboxUnb64(intent.subjectKeyB64))
                } ?: return false
            // Recomputed over the table as it is now (`adminsWithDelta`).
            is RetryableRequest.SetAdmin ->
                setMemberAdmin(groupIdHex, outboxUnb64(intent.subjectKeyB64), intent.makeAdmin)
            is RetryableRequest.TransferOwnership ->
                transferGroupOwnership(groupIdHex, outboxUnb64(intent.subjectKeyB64), intent.outgoing)
        }
        return true
    }

    // -----------------------------------------------------------------
    // ADR-0050 — per-device conversation leaves: the device mailbox, the
    // pairing / removal fan-outs and the scheduled re-admission.
    // -----------------------------------------------------------------

    // ADR-0060 §6 (amending ADR-0050 section 2.7) — this DEVICE's mailbox is
    // no longer polled: every Welcome goes to the ACCOUNT mailbox, which the
    // contacts inbox reads. The device mailbox stays claimed and closed.

    /** The hex ids of the groups this device holds a handle for. */
    public fun joinedGroupIdsHex(): Set<String> = synchronized(groups) { groups.keys.toSet() }

    /**
     * ADR-0050 / docs/ux/second-device.md section 2 — whether the chat list
     * shows the single "Synchronizing..." label: groups the pairing bundle
     * named that this device has not joined yet, or a device that landed
     * through a recovery route and holds no chat yet.
     */
    public fun isSynchronizing(): Boolean {
        val joined = joinedGroupIdsHex()
        val bundle = me.lakko.app.devices.StateBundle
        val account = currentAccountIdB64.get()
        if (bundle.pendingKnownGroups(applicationContext, account, joined) > 0) return true
        return bundle.restoredAwaitingChats(applicationContext, account, joined.size)
    }

    /** What a device fan-out did, per chat. */
    public data class DeviceFanOut(val offered: Int, val skipped: Int, val failed: Int)

    /**
     * ADR-0050 section 2.8 — admit a device this device just added to the
     * DeviceGroup (QR pairing) to EVERY chat this device is in. Waits, bounded,
     * for the new device's conversation KeyPackage to appear in the directory
     * (it publishes it once it has landed a session), then per chat a
     * self-service Add proposal swept here when this device is the acting
     * committer, or a direct commit on a table-less group.
     *
     * @throws ChatError when the KeyPackage never appeared within the wait (the
     *   device is then admitted by a later re-admission check).
     */
    public suspend fun admitDevice(deviceKey: ByteArray): DeviceFanOut =
        withContext(mlsDispatcher) { admitDeviceOnMlsThread(deviceKey = deviceKey) }

    private suspend fun admitDeviceOnMlsThread(deviceKey: ByteArray): DeviceFanOut {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped — call bootstrap() first")
        val account = keyPackageAccount.get()
            ?: throw IllegalStateException("admitDevice: no session")
        val found = awaitDeviceKeyPackage(account.accountId, deviceKey)
        // ADR-0066 section 2.3 — "add my own device" obeys the same rule as
        // the re-admission arm: only a device the account's DeviceGroup roster
        // lists. Read after the wait, so a device removed meanwhile (or one
        // that was never let in) is not put into any chat.
        // The device-sync runtime reopens its handle right after a local add;
        // a roster that is momentarily unreadable is waited for, briefly.
        var deviceRoster = ownDeviceRoster()
        var waits = 0
        while (deviceRoster == null && waits < OWN_ROSTER_WAITS) {
            delay(OWN_ROSTER_WAIT_MS)
            deviceRoster = ownDeviceRoster()
            waits += 1
        }
        if (!DeviceReadmission.ownDeviceAdmissible(DsClient.pseudonymHex(deviceKey), deviceRoster)) {
            throw ChatError(
                "the device is not in this account's device list; it is not added to any chat",
                IllegalStateException("device absent from the DeviceGroup roster"),
            )
        }
        return fanOut { groupIdHex ->
            addKeyPackageToGroup(groupIdHex, identity, found, skipIfPresent = true, accountId = account.accountId)
        }
    }

    /** Poll the directory until [deviceKey]'s KeyPackage is listed, bounded by
     *  [ADMIT_DEVICE_WAIT_MS]. */
    private suspend fun awaitDeviceKeyPackage(accountId: ByteArray, deviceKey: ByteArray): ByteArray {
        val deadline = System.currentTimeMillis() + ADMIT_DEVICE_WAIT_MS
        while (true) {
            val hit = try {
                discovery.fetchKeyPackages(accountId)
                    ?.firstOrNull { it.deviceId.contentEquals(deviceKey) }
                    ?.keyPackage
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                null
            }
            if (hit != null) return hit
            if (System.currentTimeMillis() >= deadline) {
                throw ChatError(
                    "the new device has not published a KeyPackage yet; it will be admitted to your chats later",
                    IllegalStateException("device KeyPackage absent"),
                )
            }
            delay(ADMIT_DEVICE_POLL_MS)
        }
    }

    /**
     * ADR-0050 section 2.10 — retire a device just removed from the DeviceGroup
     * from EVERY chat this device is in (its one leaf per chat, the account's
     * other devices untouched). `failed == 0` in the result means every chat
     * answered; the owed removal retries until it does (ADR-0066).
     */
    public suspend fun retireDevice(deviceKey: ByteArray): DeviceFanOut =
        withContext(mlsDispatcher) { retireDeviceOnMlsThread(deviceKey = deviceKey) }

    private suspend fun retireDeviceOnMlsThread(deviceKey: ByteArray): DeviceFanOut {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped — call bootstrap() first")
        // ADR-0066 section 2.3 — the directory row is deleted BEFORE this, by
        // the removal operation itself and with the account statement another
        // device's row needs (`DeviceRemovalRunner`, step 3). Nothing is
        // deleted here any more: a session may delete only its own row
        // without that statement.
        return fanOut { groupIdHex -> retireLeafFromGroup(groupIdHex, identity, deviceKey) }
    }

    /**
     * ADR-0066 — the device keys (hex, the roster encoding of this class) in
     * the account's DeviceGroup roster, or `null` when it cannot be read now.
     * Read from the device-sync runtime's live handle; replaceable in tests.
     */
    public var ownDeviceRoster: () -> Set<String>? = {
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.deviceRosterHex()
    }

    /** Fire-and-forget [admitDevice] on a process-lifetime scope: the wait for
     *  the new device's KeyPackage outlives the pairing screen. */
    public fun requestAdmitDevice(deviceKey: ByteArray) {
        val key = deviceKey.copyOf()
        keyPackageScope.launch {
            try {
                val result = admitDevice(key)
                android.util.Log.i("ChatRepository", "ADR-0050 admit device: $result")
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0050 admit device: fan-out failed", cause)
            }
        }
    }

    /** Fire-and-forget [retireDevice] on a process-lifetime scope. */
    public fun requestRetireDevice(deviceKey: ByteArray) {
        val key = deviceKey.copyOf()
        keyPackageScope.launch {
            try {
                val result = retireDevice(key)
                android.util.Log.i("ChatRepository", "ADR-0050 retire device: $result")
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0050 retire device: fan-out failed", cause)
            }
        }
    }

    /** Run [step] over every active group; a proposal is swept at once. */
    private suspend fun fanOut(step: suspend (String) -> MembershipOutcome?): DeviceFanOut {
        var offered = 0
        var skipped = 0
        var failed = 0
        for (groupIdHex in joinedGroupIdsHex()) {
            if (isGroupActive(groupIdHex) == false) {
                skipped += 1
                continue
            }
            try {
                val outcome = step(groupIdHex)
                if (outcome == null) {
                    skipped += 1
                    continue
                }
                offered += 1
                if (outcome is MembershipOutcome.Proposed) sweepIfCommitter(groupIdHex)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                failed += 1
                android.util.Log.w("ChatRepository", "ADR-0050 device fan-out: chat skipped $groupIdHex", cause)
            }
        }
        return DeviceFanOut(offered, skipped, failed)
    }

    /**
     * ADR-0050 — retire ONE leaf (by its signature key) from [groupIdHex]. Unlike
     * [removeMemberFromGroup], which removes the PERSON, the account's other
     * devices stay. `null` when no leaf holds that key. A direct commit on a
     * table-less group is followed by the ADR-0043 rotation like any removal.
     */
    private suspend fun retireLeafFromGroup(
        groupIdHex: String,
        identity: Identity,
        leafKey: ByteArray,
    ): MembershipOutcome? {
        val record = groups[groupIdHex] ?: return null
        val leaf = providerLock.withLock { leafIndexOfSignatureKey(record.group, leafKey) } ?: return null
        var removalLanded = false
        val step = rotationStepOrNull(groupIdHex)
        return GroupPushRouting.publishRemoveThenRotate(
            scope = pushScope,
            rotate = if (step == null) {
                null
            } else {
                suspend {
                    if (removalLanded) step() else GroupPushRouting.RotationOutcome.SKIP_NO_SHRINK
                }
            },
            onRotationError = { cause ->
                android.util.Log.w("ChatRepository", "push routing: rotation failed", cause)
            },
            publishRemove = {
                publishRemoveCommit(groupIdHex, identity, leaf).also {
                    removalLanded = it is MembershipOutcome.Committed
                }
            },
        )
    }

    /**
     * ADR-0050 section 2.4 — re-check the device lists of the accounts in
     * [groupIdHex] against its roster: propose an Add for a listed device with
     * no leaf, and — for this account only — a Remove for a leaf whose
     * directory row is gone (`forgetDevice`). Accounts resolve to directory ids
     * through this device's own account and its accepted contacts; a member
     * that is neither is left as is.
     */
    private suspend fun readmitIfDue(groupIdHex: String) {
        MainThreadGuard.assertOffMain("readmitIfDue")
        val identity = canonicalIdentity.get() ?: return
        val account = keyPackageAccount.get() ?: return
        val record = groups[groupIdHex] ?: return
        if (isGroupActive(groupIdHex) == false) return
        if (!DeviceReadmission.takeSlot(groupIdHex, System.currentTimeMillis())) return
        val snapshot = providerLock.withLock {
            val leaves = try {
                record.group.members()
            } catch (_: Throwable) {
                null
            }
            leaves?.let { it to ownIdentity(record.group) }
        } ?: return
        val (leaves, ownPerson) = snapshot
        if (ownPerson == null) return
        val roster = leaves.map {
            DeviceReadmission.RosterLeaf(
                leafKey = DsClient.pseudonymHex(it.signaturePublicKey),
                personKey = outboxB64(personKeyOf(it)),
            )
        }
        val people = roster.map { it.personKey }.toSet()
        val lookups = mutableListOf(ownPerson to account.accountId)
        // ADR-0065 phase 2 / UX 16.1 point 2 — the reset records this account
        // holds: a contact flagged (or whose previous key is still in the
        // roster) is matched by EITHER key, and its ADD branch is suppressed.
        val resetRecords = keyStateStore.loadAll(account.accountIdB64).associateBy { it.accountIdB64 }
        val suppressedAccounts = HashSet<String>()
        for (c in me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext, account.accountId)) {
            val id = c.request.fromAccountId ?: continue
            val idB64 = outboxB64(id)
            val record = resetRecords[idB64]
            val person = outboxB64(c.request.fromPubkey)
            val inRoster = person in people ||
                (record != null && (record.previousKeyB64 in people || record.pendingKeyB64 in people))
            if (!inRoster || person == ownPerson) continue
            // 16.1 point 2 — the arm also fetches and checks the account's
            // published identity, so a plain member who never starts anything
            // with the contact still learns of the reset within the interval.
            // The check records a logged reset through the contacts repository;
            // its answer here is read back from the store it writes.
            contactIdentityCheck?.let { check ->
                try {
                    check(id)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (_: Throwable) {
                    // best-effort
                }
            }
            val after = keyStateStore.load(account.accountIdB64, idB64) ?: record
            // 16.8 — the published key is not the pin while the contact is
            // flagged; a verified contact's pin IS the published key by the
            // confirmation that wrote it, so the predicate reads off the record.
            if (suppressReadmissionAdd(contactUnverified = after?.unverified == true, publishedKeyIsPinned = after?.let { !it.unverified })) {
                suppressedAccounts += idB64
                continue
            }
            lookups += person to id
        }
        // The account id each listed device was fetched under, so an Add
        // proposed below records the id its KeyPackage is fetched again by
        // (ADR-0050 section 2.5a re-request).
        val accountIdOfDevice = HashMap<String, ByteArray>()
        val accounts = lookups.map { (person, id) ->
            val devices = try {
                discovery.fetchKeyPackages(id).orEmpty().map {
                    val hex = DsClient.pseudonymHex(it.deviceId)
                    accountIdOfDevice[hex] = id
                    DeviceReadmission.DirectoryDevice(hex, it.keyPackage)
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                null
            }
            DeviceReadmission.AccountDevices(person, devices)
        }
        if (suppressedAccounts.isNotEmpty()) {
            android.util.Log.i(
                "ChatRepository",
                "ADR-0065 re-admission: add branch suppressed for ${suppressedAccounts.size} reset contact(s) in $groupIdHex",
            )
        }
        val plan = DeviceReadmission.plan(
            roster = roster,
            ownPersonKey = ownPerson,
            accounts = accounts,
            removed = DeviceReadmission.removed(applicationContext, account.accountIdB64, groupIdHex),
            // ADR-0066 — own devices only from the DeviceGroup roster.
            ownDeviceRoster = ownDeviceRoster(),
        )
        if (plan.admit.isEmpty() && plan.retire.isEmpty()) return
        var proposed = 0
        for (d in plan.admit) {
            try {
                val accountId = accountIdOfDevice[d.deviceKey] ?: continue
                if (addKeyPackageToGroup(groupIdHex, identity, d.keyPackage, skipIfPresent = true, accountId = accountId) != null) {
                    proposed += 1
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0050 re-admission: add not proposed", cause)
            }
        }
        val ownLeafHex = DsClient.pseudonymHex(identity.signaturePublicKey())
        for (leafHex in plan.retire) {
            // Never this device's own leaf: its directory row can be missing only
            // because its own KeyPackage upload has not landed yet (session start
            // races the first poll), and retiring it would evict this device.
            if (leafHex == ownLeafHex) continue
            val key = hexToBytes(leafHex) ?: continue
            try {
                if (retireLeafFromGroup(groupIdHex, identity, key) != null) proposed += 1
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0050 re-admission: retire not proposed", cause)
            }
        }
        if (proposed > 0) {
            android.util.Log.i(
                "ChatRepository",
                "ADR-0050 re-admission: ${plan.admit.size} to admit, ${plan.retire.size} to retire in $groupIdHex",
            )
            sweepIfCommitter(groupIdHex)
        }
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
     * @param exclude groups another loop is already long-polling (an open chat
     *   screen, [screenPolledGroups]). Polling one of them here would lose the
     *   per-group in-flight race and return at once, so a loop whose every
     *   group is excluded would spin instead of waiting.
     * @return how many groups the pass polled and how many answered, which the
     *   app-level receive loop reports into [feedRefresh] (`docs/ux/feed.md`).
     */
    public suspend fun pollAllGroups(
        waitSecs: Int = 0,
        exclude: Set<String> = emptySet(),
    ): me.lakko.app.feed.SourceCheck =
        withContext(mlsDispatcher) { pollAllGroupsOnMlsThread(waitSecs = waitSecs, exclude = exclude) }

    /**
     * [pollAllGroups] for the feed's pull to refresh (`docs/ux/feed.md`
     * section 5): the same best-effort pass, reporting how many groups it
     * polled and how many answered, so the feed can tell "the network is not
     * there" from one bad group.
     */
    public suspend fun pollAllGroupsReporting(): me.lakko.app.feed.SourceCheck =
        withContext(mlsDispatcher) { pollAllGroupsOnMlsThread(waitSecs = 0) }

    private suspend fun pollAllGroupsOnMlsThread(
        waitSecs: Int = 0,
        exclude: Set<String> = emptySet(),
    ): me.lakko.app.feed.SourceCheck {
        if (!mutableState.value.bootstrapped) return me.lakko.app.feed.SourceCheck.NONE
        canonicalIdentity.get() ?: return me.lakko.app.feed.SourceCheck.NONE
        // Snapshot the keys under the map's own monitor — `groups` is a
        // `synchronizedMap`, whose iterators are NOT self-synchronising.
        val groupIds = synchronized(groups) { groups.keys.toList() }.filter { it !in exclude }
        // ADR-0045 UX §6.2.8 item 1 — filtered HERE as well as gated inside
        // [pollGroup], and the reason is a spin rather than a duplicate check:
        // the app-level receive loop drives this in `while (true) { … }` and
        // relies on the long poll for its cadence, so a fleet of groups this
        // device has all been removed from would return instantly, forever.
        // Filtering first lets the empty case take one honest long-poll-shaped
        // pause instead.
        val active = groupIds.filter { isGroupActive(it) != false }
        if (active.isEmpty()) {
            if (waitSecs > 0) delay(waitSecs.toLong() * MILLIS_PER_SECOND)
            return me.lakko.app.feed.SourceCheck.NONE
        }
        // docs/ux/calls.md section 2.1 — the fan-out is bounded, so a set larger
        // than [POLL_ALL_CONCURRENCY] polls in WAVES and one sweep costs one
        // wait per wave. At a fixed 30 s that put a group in the second wave out
        // of reach of a 45 s ring window, so an incoming call rang only in the
        // one chat whose screen was open and polling it directly. The wait is
        // therefore divided across the waves; a single-wave set is unchanged.
        // See [appPollWaitSecs].
        val perGroupWait = appPollWaitSecs(
            groupCount = active.size,
            concurrency = POLL_ALL_CONCURRENCY,
            maxWaitSecs = waitSecs,
        )
        val answered = runPollAllGroups(
            groupIds = active,
            concurrency = POLL_ALL_CONCURRENCY,
        ) { groupIdHex ->
            pollGroup(groupIdHex, waitSecs = perGroupWait)
        }
        return me.lakko.app.feed.SourceCheck(tried = active.size, answered = answered)
    }

    /**
     * Clear the in-memory state for the current session. Keeps
     * the on-disk MLS snapshot intact — `forgetDevice` is the
     * full-wipe path. Called from `AuthRepository.logout`.
     */
    public fun clear() {
        introductionTicker?.cancel()
        introductionTicker = null
        me.lakko.app.reach.ReachManager.peek()?.clear()
        DeviceReadmission.clearSlots()
        wipeInMemoryState()
        closedStates.clear()
        evictionsProposedThisSession.clear()
        resetRootHandoverState()
        localUnread.clear()
        keyPackageAccount.set(null)
        snapshotUnreadable.set(false)
        currentAccountIdB64.set(null)
        canonicalIdentity.set(null)
        accountIdentity.getAndSet(null)?.close()
        publishedAccountIdentity.getAndSet(null)?.close()
        // ADR-0059 section 6 — the in-memory shared-channels row (the sealed
        // file stays for the next sign-in unless forget-device deletes it).
        sharedChannels.clear()
        feedRefresh.reset()
        mutableState.value = ChatRepositoryState()
    }

    /**
     * Forget device / Delete account, the chat half: take this device's
     * KeyPackage out of the directory, then clear the in-memory state like
     * [clear] does. Nothing on disk is touched here: every per-account store
     * this repository writes is removed by
     * [me.lakko.app.account.AccountLocalWipe], which `AuthRepository.forgetDevice`
     * runs after this. Call it BEFORE that (it reads the live session).
     */
    public fun forgetDevice() {
        // ADR-0050 section 2.10 — take this device's KeyPackage out of the
        // directory, with the values captured before the wipe below: the
        // account's other devices retire this device's leaves from every chat
        // when their scheduled check sees the row gone, and nobody proposes a
        // Welcome to a device that no longer exists. Launched (this entry point
        // is not suspending); it uses only the captured key and token.
        val ownLeafKey = try {
            canonicalIdentity.get()?.signaturePublicKey()
        } catch (_: Throwable) {
            null
        }
        val token = keyPackageAccount.get()?.sessionToken
        if (ownLeafKey != null && token != null) {
            keyPackageScope.launch {
                try {
                    keyPackageDiscoveryFactory().use { client ->
                        client.setSessionToken(token)
                        client.deleteKeyPackage(ownLeafKey)
                    }
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "forgetDevice: directory KeyPackage not deleted", cause)
                }
            }
        }
        clear()
        lastBeaconAtMs.clear()
    }

    override fun close() {
        // A client never built has nothing to close (and building one just to
        // close it would cost the engine it was made lazy to avoid).
        if (chatClientLazy.isInitialized()) chatClient.close()
        if (dsClientLazy.isInitialized()) dsClient.close()
        if (discoveryLazy.isInitialized()) discovery.close()
        canonicalIdentity.getAndSet(null)?.close()
        accountIdentity.getAndSet(null)?.close()
        publishedAccountIdentity.getAndSet(null)?.close()
        synchronized(groups) {
            for (rec in groups.values) rec.group.close()
            groups.clear()
        }
        dsSigners.clearOwner(DS_SIGNER_OWNER)
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
        // ADR-0053 — the journal and mailbox signers belong to the session.
        dsSigners.clearOwner(DS_SIGNER_OWNER)
        // ADR-0048 — per-group republish timing belongs to the session.
        profileRepublish.reset()
        // ADR-0051 — and the group-name re-send timing with it.
        groupNameRepublish.reset()
        // ADR-0052 decision 3 — and the pin re-send timing.
        pinRepublish.reset()
        // A previous session's group-call records name calls in groups this
        // account may not be in.
        mutableGroupCalls.value = emptyMap()
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
        MainThreadGuard.assertOffMain("persistSnapshotBestEffort")
        val identity = canonicalIdentity.get() ?: return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            val blob = identity.snapshotStorage()
            writeSnapshot(identity, accountIdB64, blob)
            snapshotUnreadable.set(false)
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
        MainThreadGuard.assertOffMain("persistSnapshotDurableOrThrow")
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "cannot persist MLS snapshot — no canonical Identity",
            )
        val accountIdB64 = currentAccountIdB64.get()
            ?: throw IllegalStateException(
                "cannot persist MLS snapshot — no account id",
            )
        val blob = identity.snapshotStorage()
        writeSnapshot(identity, accountIdB64, blob)
        snapshotUnreadable.set(false)
    }

    /** Write [blob] and record the leaf key it was taken under (ADR-0050). */
    private fun writeSnapshot(identity: Identity, accountIdB64: String, blob: ByteArray) {
        snapshotStore.putSnapshot(applicationContext, accountIdB64, blob)
        snapshotStore.putSnapshotLeafKey(applicationContext, accountIdB64, identity.signaturePublicKey())
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
    // -----------------------------------------------------------------
    // ADR-0053 / ADR-0057 — the conversation journals and the two own
    // mailboxes are claimed; every request to them carries a proof. A group
    // journal has an OWNER (the ADR-0022 owner's account) holding its root
    // key; epoch keys are registered only by the root or the registrars it
    // appoints.
    // -----------------------------------------------------------------

    /** Where [DsClient] finds the signer of each journal this repository owns. */
    private val dsSigners: DsSignerRegistry = DsSignerRegistry.shared

    /** ADR-0057 — registrar seeds this account was appointed with, and the
     *  appointments this account made as an owner. */
    private val journalOwnershipStore: me.lakko.app.ds.EncryptedJournalOwnershipStore by lazy {
        me.lakko.app.ds.EncryptedJournalOwnershipStore(applicationContext)
    }

    /** ADR-0057 — one reconcile at a time per group (appointments are a
     *  read-diff-write against the stored set). */
    private val appointmentLocks = java.util.concurrent.ConcurrentHashMap<String, Mutex>()

    /** ADR-0057 — when an incomplete reconcile (a member whose X25519 key could
     *  not be resolved yet) last ran, per group, so the poll tail does not
     *  repeat its network lookups on every poll. */
    private val appointmentAttemptAtMs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** Finding 2 — the registrar-set fingerprint this session last sent per
     *  group (sent once per group per session and on every change). */
    private val sentRegistrarSets = java.util.concurrent.ConcurrentHashMap<String, String>()

    /** Finding 4 — when an owned group without the root-held marker may probe
     *  again, per group. */
    private val rootProbeNotBeforeMs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** Finding 1 — the repair scheduler for conversation-group journals. */
    private val journalRepair: me.lakko.app.ds.JournalRepair by lazy {
        me.lakko.app.ds.JournalRepair(
            scope = pushScope,
            ports = object : me.lakko.app.ds.JournalRepairPorts {
                override suspend fun rank(groupIdHex: String): me.lakko.app.ds.JournalRepairRank {
                    val record = groups[groupIdHex] ?: return me.lakko.app.ds.JournalRepairRank.NONE
                    return providerLock.withLock { journalRepairRankFor(record, groupIdHex) }
                }

                override fun holds(groupIdHex: String): Boolean = groups[groupIdHex] != null

                override suspend fun pollToHead(groupIdHex: String) {
                    val record = groups[groupIdHex] ?: return
                    repeat(JOURNAL_REPAIR_MAX_POLLS) {
                        val before = record.cursor
                        val outcome = pollGroup(groupIdHex, waitSecs = 0, joinInFlight = true)
                        if (outcome == GroupPollOutcome.UNAVAILABLE) return
                        if (outcome == GroupPollOutcome.APPLIED && record.cursor == before) return
                    }
                }

                override fun currentEpochKey(groupIdHex: String): me.lakko.app.ds.EpochKey? =
                    currentJournalSigner(groupIdHex)?.let { me.lakko.app.ds.EpochKey(it.epoch, it.verifyingKey) }

                override suspend fun repairSigner(groupIdHex: String): me.lakko.app.ds.DsStatementSigning? {
                    val record = groups[groupIdHex] ?: return null
                    return providerLock.withLock {
                        if (me.lakko.app.ds.mayRepair(journalRepairRankFor(record, groupIdHex))) {
                            authoritySignerFor(record, groupIdHex)
                        } else {
                            null
                        }
                    }
                }

                override suspend fun put(make: () -> me.lakko.app.ds.DsStatement) {
                    dsClient.putStatement(make)
                }
            },
            log = { line -> android.util.Log.i("ChatRepository", "ADR-0057 repair: $line") },
        )
    }

    /** This session's ACCOUNT key (the ADR-0022 / ADR-0050 identity a role
     *  table names), or `null` before bootstrap. */
    private fun ownAccountKey(): ByteArray? =
        try {
            accountIdentity.get()?.signaturePublicKey()
        } catch (_: Throwable) {
            null
        }

    /** The live role table's owner and admins, or `null` for a group with no
     *  table or an unreadable one. MUST be read on the provider critical path. */
    private fun roleTableOrNull(group: Group): uniffi.lakko_ffi_uniffi.RoleTable? =
        try {
            group.groupRoles()
        } catch (_: Throwable) {
            null
        }

    /** ADR-0057 — does this account own [group]'s journal? */
    private fun ownsJournal(group: Group): Boolean {
        val own = ownAccountKey() ?: return false
        val owner = roleTableOrNull(group)?.owner ?: return false
        return owner.contentEquals(own)
    }

    /** ADR-0057 — the journal's root signer, derived from the ACCOUNT identity
     *  (every device of the owner's account derives the same one). */
    private fun rootStatementSigner(groupId: ByteArray): me.lakko.app.ds.DsStatementSigning? {
        val account = rootAccountIdentity() ?: return null
        return try {
            me.lakko.app.ds.FfiDsStatementSigning.root(account, groupId)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0057: root signer unavailable", cause)
            null
        }
    }

    /** ADR-0057 — this account's registrar signer for [groupIdHex], when the
     *  owner appointed it. */
    private fun registrarStatementSigner(groupIdHex: String): me.lakko.app.ds.DsStatementSigning? {
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        val seed = try {
            journalOwnershipStore.get(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            null
        } ?: return null
        return try {
            me.lakko.app.ds.FfiDsStatementSigning.ofSeed(seed)
        } catch (_: Throwable) {
            null
        } finally {
            seed.fill(0)
        }
    }

    /** ADR-0057 obligation 5 — who signs the registration of a commit on
     *  [record]'s group: the root (owner), the registrar (appointed member),
     *  or nobody (this device must not commit). Provider critical path. */
    private fun commitStatementSignerFor(
        record: GroupRecord,
        groupIdHex: String,
    ): me.lakko.app.ds.DsStatementSigning? =
        me.lakko.app.ds.commitStatementSigner(
            isOwner = ownsJournal(record.group),
            root = { rootStatementSigner(record.groupId) },
            registrar = { registrarStatementSigner(groupIdHex) },
            rootHeld = isRootHeld(groupIdHex),
        )

    /** ADR-0057 second security pass, finding 4 — whether this account is
     *  known to hold [groupIdHex]'s journal root on the DS. */
    private fun isRootHeld(groupIdHex: String): Boolean {
        val accountIdB64 = currentAccountIdB64.get() ?: return false
        return try {
            journalOwnershipStore.rootHeld.isHeld(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            false
        }
    }

    private fun markRootHeld(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            journalOwnershipStore.rootHeld.mark(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            // Best effort: a lost marker costs one probe.
        }
        rootProbeNotBeforeMs.remove(groupIdHex)
    }

    private fun clearRootHeld(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            journalOwnershipStore.rootHeld.clear(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            // best effort
        }
        rootProbeNotBeforeMs.remove(groupIdHex)
    }

    /** Which key this device signs [record]'s journal statements with.
     *  Provider critical path (reads the role table). */
    private fun journalAuthorityKindFor(record: GroupRecord, groupIdHex: String): me.lakko.app.ds.JournalAuthorityKind =
        me.lakko.app.ds.journalAuthorityKind(
            isOwner = ownsJournal(record.group),
            rootHeld = isRootHeld(groupIdHex),
            registrar = { registrarSeedPresent(groupIdHex).takeIf { it } },
        )

    private fun registrarSeedPresent(groupIdHex: String): Boolean {
        val accountIdB64 = currentAccountIdB64.get() ?: return false
        return try {
            journalOwnershipStore.get(accountIdB64, groupIdHex)?.also { it.fill(0) } != null
        } catch (_: Throwable) {
            false
        }
    }

    /** Finding 1 — this device's standing for the repair: the root, an admin
     *  registrar (the role table names this account admin AND it holds a
     *  registrar seed), a member registrar, or nothing. Provider critical path. */
    private fun journalRepairRankFor(record: GroupRecord, groupIdHex: String): me.lakko.app.ds.JournalRepairRank =
        when (journalAuthorityKindFor(record, groupIdHex)) {
            me.lakko.app.ds.JournalAuthorityKind.ROOT -> me.lakko.app.ds.JournalRepairRank.ROOT
            me.lakko.app.ds.JournalAuthorityKind.REGISTRAR -> {
                val own = ownAccountKey()
                val admin = own != null && roleTableOrNull(record.group)?.admins?.any { it.contentEquals(own) } == true
                // An owner still signing with its registrar seed was appointed
                // by the previous owner, usually at admin rank.
                if (admin || ownsJournal(record.group)) {
                    me.lakko.app.ds.JournalRepairRank.ADMIN
                } else {
                    me.lakko.app.ds.JournalRepairRank.MEMBER
                }
            }
            me.lakko.app.ds.JournalAuthorityKind.NONE -> me.lakko.app.ds.JournalRepairRank.NONE
        }

    /** The signer of [record]'s journal statements for this device's
     *  authority kind, or `null`. Provider critical path. */
    private fun authoritySignerFor(record: GroupRecord, groupIdHex: String): me.lakko.app.ds.DsStatementSigning? =
        when (journalAuthorityKindFor(record, groupIdHex)) {
            me.lakko.app.ds.JournalAuthorityKind.ROOT -> rootStatementSigner(record.groupId)
            me.lakko.app.ds.JournalAuthorityKind.REGISTRAR -> registrarStatementSigner(groupIdHex)
            me.lakko.app.ds.JournalAuthorityKind.NONE -> null
        }

    /**
     * ADR-0057 — the registrar public key of each evictee, put in the
     * registration of the commit that removes them.
     *
     *   * The ROOT derives every key itself.
     *   * Anyone else ("Who revokes", amended 2026-10-01) — an admin registrar,
     *     or an owner whose takeover is not confirmed and still signs with its
     *     admin-rank registrar seed — revokes from the registrar list it
     *     collected from the seed envelopes it read, keeping only keys of a
     *     rank STRICTLY below its own: the DS refuses a revocation of an equal
     *     or higher rank, so an admin's removal of another admin leaves that
     *     key to the root (the owner's next `registrars-set`), and a
     *     member-rank registrar revokes nothing.
     *
     * Provider critical path (reads the role table).
     */
    private fun revokeKeysFor(record: GroupRecord, groupIdHex: String, evictees: List<ByteArray>): List<ByteArray> {
        if (evictees.isEmpty()) return emptyList()
        if (!ownsJournal(record.group) ||
            journalAuthorityKindFor(record, groupIdHex) != me.lakko.app.ds.JournalAuthorityKind.ROOT
        ) {
            return revokeKeysFromRegistrarList(record, groupIdHex, evictees)
        }
        val account = rootAccountIdentity() ?: return emptyList()
        val rootSeed = try {
            uniffi.lakko_ffi_uniffi.dsJournalRootSeed(account.ed25519Seed(), record.groupId)
        } catch (_: Throwable) {
            return emptyList()
        }
        return try {
            me.lakko.app.ds.revokeListFor(evictees, ownAccountKey()) { accountKey ->
                val seed = uniffi.lakko_ffi_uniffi.dsJournalRegistrarSeed(rootSeed, accountKey)
                try {
                    uniffi.lakko_ffi_uniffi.dsStatementSigner(seed).use { it.publicKey() }
                } finally {
                    seed.fill(0)
                }
            }
        } finally {
            rootSeed.fill(0)
        }
    }

    /** The non-root half of [revokeKeysFor]: the listed keys of [evictees]
     *  this device's registrar rank strictly outranks. */
    private fun revokeKeysFromRegistrarList(
        record: GroupRecord,
        groupIdHex: String,
        evictees: List<ByteArray>,
    ): List<ByteArray> {
        val accountIdB64 = currentAccountIdB64.get() ?: return emptyList()
        val signerRank = when (journalRepairRankFor(record, groupIdHex)) {
            me.lakko.app.ds.JournalRepairRank.ADMIN -> me.lakko.app.ds.JournalRank.ADMIN
            me.lakko.app.ds.JournalRepairRank.MEMBER -> me.lakko.app.ds.JournalRank.MEMBER
            else -> null
        }
        val listed = try {
            journalOwnershipStore.registrarList.get(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            emptyMap()
        }
        return me.lakko.app.ds.revokeListFromRegistrarList(evictees, ownAccountKey(), signerRank, listed)
    }

    /** The roster as the ownership policy reads it (leaf -> account key), or
     *  `null` on a torn read. Provider critical path. */
    private fun rosterLeavesOrNull(group: Group): List<me.lakko.app.ds.RosterLeaf>? =
        try {
            group.members().map { me.lakko.app.ds.RosterLeaf(it.leafIndex, it.accountKey) }
        } catch (_: Throwable) {
            null
        }

    /**
     * Teach the shared registry how to sign for [groupIdHex]'s journal: the
     * signer is derived from whatever [Group] handle the record holds NOW (an
     * ADR-0040 reset swaps the handle, and the lookup follows it) at the
     * identity of this session. Every `DsClient` in the process then signs
     * this journal's requests without the call site passing anything.
     *
     * ADR-0057 obligation 2 — only when this account OWNS the journal does the
     * source carry the lazy-claim hook a signed request's `401` runs; a plain
     * member never claims.
     */
    private fun installJournalSigner(groupIdHex: String) {
        dsSigners.install(
            dsGroupKeyHex(groupIdHex),
            object : DsSignerSource {
                override fun epoch(): ULong? = groups[groupIdHex]?.group?.epoch()

                override fun derive(): DsJournalSigner? {
                    val identity = canonicalIdentity.get() ?: return null
                    val group = groups[groupIdHex]?.group ?: return null
                    return FfiDsJournalSigner(group.dsAccessSigner(identity))
                }

                override val ownerClaim: (suspend () -> Boolean)?
                    get() {
                        val group = groups[groupIdHex]?.group ?: return null
                        val owner = try {
                            ownsJournal(group)
                        } catch (_: Throwable) {
                            false
                        }
                        return if (owner) ({ claimJournalAsOwner(groupIdHex) }) else null
                    }

                // ADR-0057 second security pass, finding 1 — a signed request
                // refused at this device's epoch notes the group for the
                // repair check, which decides (under the lock, 30 s later)
                // whether this device may repair at all.
                override val onRefusedAtHead: ((ULong) -> Unit)
                    get() = { epoch ->
                        groups[groupIdHex]?.let { record ->
                            journalRepair.note(
                                groupIdHex = groupIdHex,
                                journalId = record.groupId,
                                epoch = epoch,
                                reason = me.lakko.app.ds.RepairReason.REFUSED_AT_HEAD,
                            )
                        }
                    }
            },
            owner = DS_SIGNER_OWNER,
        )
    }

    /** The signer of [groupIdHex]'s journal at the epoch its handle holds now. */
    private fun currentJournalSigner(groupIdHex: String): DsJournalSigner? =
        dsSigners.current(dsGroupKeyHex(groupIdHex))

    /**
     * ADR-0057 obligations 1 and 2 — the OWNER claims the journal with its
     * root at the current epoch (idempotent: `204` when that exact claim is
     * already registered; `409` when the journal carries another chain, which
     * is left alone — a chat journal is never reset). Answers whether the claim
     * landed. A non-owner returns `false` without a request.
     */
    private suspend fun claimJournalAsOwner(groupIdHex: String): Boolean {
        val record = groups[groupIdHex] ?: return false
        val (root, epoch, epochKey) = providerLock.withLock {
            if (!ownsJournal(record.group)) return false
            val root = rootStatementSigner(record.groupId) ?: return false
            val signer = currentJournalSigner(groupIdHex) ?: return false
            Triple(root, signer.epoch, signer.verifyingKey)
        }
        val outcome = me.lakko.app.ds.claimOrReset(
            journalId = record.groupId,
            epoch = epoch,
            epochKey = epochKey,
            statements = root,
            refounded = false,
            put = { make -> dsClient.putStatement(make) },
        )
        android.util.Log.i("ChatRepository", "ADR-0057: owner claim $outcome")
        if (outcome == me.lakko.app.ds.JournalClaimOutcome.CLAIMED) {
            markRootHeld(groupIdHex)
            // The journal is claimed now: a sweep held after a refused
            // registration may try again at once.
            sweepBackoff.reset(groupIdHex)
        }
        return outcome == me.lakko.app.ds.JournalClaimOutcome.CLAIMED
    }

    /**
     * ADR-0057 obligation 2 — the lazy claim on open: the owner (only) claims
     * its journal with the root, which is what re-claims every group the DS
     * flag day left unclaimed, then reconciles its registrar appointments.
     * Launched, never awaited, never surfaced: a failure retries on the next
     * open.
     */
    private fun claimJournalBestEffort(groupIdHex: String) {
        if (groups[groupIdHex] == null) return
        // Sent on every open, not only after a signed request's 401: a journal
        // the ADR-0057 flag day left unclaimed is OPEN, and an open journal
        // never answers 401, so a lazy-only claim would never be made. Tracked
        // by [ownerClaimGate] so a sweep registers its epoch after it.
        val claim = ownerClaimGate.start(groupIdHex, pushScope) {
            try {
                claimJournalAsOwner(groupIdHex)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0057: claim of a group journal did not land", cause)
                false
            }
        }
        pushScope.launch {
            claim.join()
            // Finding 2 — the owner converges the registrar set once per group
            // per session (a no-op for a non-owner), whatever the claim said: a
            // live journal answers the lazy claim with 409 and still needs it.
            try {
                reconcileAppointments(groupIdHex, force = true)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0057: appointment reconcile failed", cause)
            }
        }
    }

    /** The registration half of [installJournalSigner] + [claimJournalBestEffort]:
     *  what every site that puts a record into [groups] does next. */
    private fun openJournal(groupIdHex: String) {
        installJournalSigner(groupIdHex)
        claimJournalBestEffort(groupIdHex)
        // ADR-0057 S2 finding 1 — a root handover still owed from an earlier
        // session resumes when its group opens, and (second security pass,
        // finding 4) so does a takeover whose old root seed is still pending.
        scheduleRootHandover(groupIdHex)
        scheduleTakeover(groupIdHex)
    }

    /**
     * ADR-0057 obligation 5 for a DIRECT (eager-merged) commit: register epoch
     * N+1's key with this account's root or registrar signer, revoking the
     * registrar keys of the accounts the commit evicted ([rosterBefore] against
     * the post-merge roster, owner only), and return the N signer the commit
     * POST is signed with. On any failure — no signer at all included — the
     * group is reset to its durable snapshot and the commit aborts: no outbox
     * entry, no publish.
     *
     * MUST run inside the provider lock, between the create-commit and the
     * outbox write.
     */
    private suspend fun registerNextEpochOrReset(
        groupIdHex: String,
        groupId: ByteArray,
        before: DsJournalSigner?,
        rosterBefore: List<me.lakko.app.ds.RosterLeaf>? = null,
    ): DsJournalSigner =
        try {
            registerOrReset(reset = { resetGroupToDurableSnapshotOrThrow(groupIdHex) }) {
                val record = groups[groupIdHex]
                    ?: throw me.lakko.app.ds.DsRegistrationUnavailable("no group record")
                val after = rosterLeavesOrNull(record.group).orEmpty().map { it.accountKey }
                val evictees = me.lakko.app.ds.evictedBetween(
                    before = rosterBefore.orEmpty().map { it.accountKey },
                    after = after,
                )
                val revoke = revokeKeysFor(record, groupIdHex, evictees)
                val rank = journalRepairRankFor(record, groupIdHex)
                noteAppendConflict(groupIdHex, groupId, before, rank) {
                    me.lakko.app.ds.registerEpochBeforePublish(
                        journalId = groupId,
                        before = before,
                        after = currentJournalSigner(groupIdHex),
                        statements = commitStatementSignerFor(record, groupIdHex),
                        revoke = revoke,
                        put = { make -> dsClient.putStatement(make) },
                    )
                }.also { forgetRevokedAppointments(groupIdHex, evictees, revoke) }
            }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: ChatError) {
            throw cause
        } catch (cause: Throwable) {
            throw ChatError(
                dsErrorMessage(cause, "The server did not accept the group change. Nothing was sent."),
                cause,
            )
        }

    /**
     * ADR-0057 second security pass, finding 1 — run one Append registration
     * of a commit path. A `409` means another committer registered that epoch
     * first: it propagates (the caller abandons the commit exactly as a lost
     * publish race; a commit path never sends a Replace), and an owner or
     * admin ([rank]) notes the group at the epoch it stays at ([before]) for
     * the repair check.
     */
    private suspend fun <T> noteAppendConflict(
        groupIdHex: String,
        groupId: ByteArray,
        before: DsJournalSigner?,
        rank: me.lakko.app.ds.JournalRepairRank,
        register: suspend () -> T,
    ): T =
        try {
            register()
        } catch (cause: DsHttpError) {
            if (cause.status == HTTP_CONFLICT_STATUS && before != null) {
                android.util.Log.i("ChatRepository", "ADR-0057: append at epoch ${before.epoch + 1uL} lost (409)")
                journalRepair.note(
                    groupIdHex = groupIdHex,
                    journalId = groupId,
                    epoch = before.epoch,
                    reason = me.lakko.app.ds.RepairReason.APPEND_CONFLICT,
                    rank = rank,
                )
            }
            throw cause
        }

    /** A registration that revoked [evictees]' keys also removed them from
     *  the DS registrar set, so the owner's appointment record drops them. */
    private fun forgetRevokedAppointments(groupIdHex: String, evictees: List<ByteArray>, revoke: List<ByteArray>) {
        if (revoke.isEmpty()) return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val gone = evictees.map { me.lakko.app.ds.hexOf(it) }.toHashSet()
        try {
            val store = journalOwnershipStore.appointments
            // The record is keyed per device ("Who revokes" item 2); the
            // account part names the evictee.
            store.put(
                accountIdB64,
                groupIdHex,
                store.get(accountIdB64, groupIdHex).filterKeys { me.lakko.app.ds.accountHexOfDeliveryKey(it) !in gone },
            )
        } catch (_: Throwable) {
            // The next reconcile removes them again (idempotent on the DS).
        }
        try {
            val list = journalOwnershipStore.registrarList
            val listed = list.get(accountIdB64, groupIdHex)
            if (listed.keys.any { it in gone }) list.put(accountIdB64, groupIdHex, listed.filterKeys { it !in gone })
        } catch (_: Throwable) {
            // A stale list entry only names a key the DS no longer holds.
        }
    }

    /**
     * ADR-0057 obligation 3, as amended by the second security pass (finding
     * 2) — the owner CONVERGES the DS registrar set to the live group: it
     * sends `registrars-set` with the whole set the roster and role table call
     * for (every other member account, admins at admin rank) whenever that set
     * differs from the one this session last sent for the group — so once per
     * group per session and on every roster or role change — then seals each
     * member's registrar seed that is not yet delivered at its rank. The local
     * record only tracks delivered envelopes; nothing depends on a local
     * ledger of what the DS holds.
     *
     * Finding 4 — an owner not yet known to hold the journal root (a takeover
     * that has not landed) probes first ([probeRootIfUnheld]) and appoints
     * nothing while the probe says the root is not its own. One reconcile per
     * group at a time; a non-owner returns at once. An incomplete run is
     * retried no sooner than [APPOINTMENT_RETRY_INTERVAL_MS] unless [force]d.
     */
    private suspend fun reconcileAppointments(groupIdHex: String, force: Boolean = false) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        val now = System.currentTimeMillis()
        if (!force) {
            val last = appointmentAttemptAtMs[groupIdHex]
            if (last != null && now - last < APPOINTMENT_RETRY_INTERVAL_MS) return
        }
        appointmentLocks.getOrPut(groupIdHex) { Mutex() }.withLock {
            val snapshot = providerLock.withLock {
                if (readIsActive { record.group.isActive() } == false) return
                if (!ownsJournal(record.group)) return
                val table = roleTableOrNull(record.group) ?: return
                val roster = rosterLeavesOrNull(record.group) ?: return
                // ADR-0057 "Who revokes" item 2 — the devices of each account,
                // so the delivery record is kept per device.
                val devices = try {
                    record.group.members().groupBy(
                        { me.lakko.app.ds.hexOf(it.accountKey) },
                        { me.lakko.app.ds.hexOf(it.signaturePublicKey) },
                    )
                } catch (_: Throwable) {
                    return
                }
                Triple(table, roster, devices)
            }
            if (!probeRootIfUnheld(groupIdHex, record)) return
            val (table, roster, devices) = snapshot
            val desired = me.lakko.app.ds.desiredRegistrars(
                memberAccounts = roster.map { it.accountKey },
                ownerAccountKey = table.owner,
                admins = table.admins,
            )
            val devicesOf: (String) -> Collection<String> = { hex -> devices[hex].orEmpty() }
            val store = journalOwnershipStore.appointments
            val delivered = store.get(accountIdB64, groupIdHex)
            val lastSent = sentRegistrarSets[groupIdHex]
            if (lastSent == me.lakko.app.ds.registrarSetFingerprint(desired) &&
                delivered == me.lakko.app.ds.expectedDeliveries(desired, devicesOf)
            ) {
                return
            }
            val root = rootStatementSigner(record.groupId) ?: return
            val account = rootAccountIdentity() ?: return
            val rootSeed = uniffi.lakko_ffi_uniffi.dsJournalRootSeed(account.ed25519Seed(), record.groupId)
            try {
                val result = me.lakko.app.ds.reconcileAppointments(
                    journalId = record.groupId,
                    desired = desired,
                    delivered = delivered,
                    lastSentSet = lastSent,
                    root = root,
                    registrarSeedFor = { accountKey ->
                        uniffi.lakko_ffi_uniffi.dsJournalRegistrarSeed(rootSeed, accountKey)
                    },
                    registrarPublicKeyOf = { seed ->
                        uniffi.lakko_ffi_uniffi.dsStatementSigner(seed).use { it.publicKey() }
                    },
                    memberX25519 = { accountKey -> memberX25519KeyOrNull(accountKey) },
                    put = { make -> dsClient.putStatement(make) },
                    sendEnvelope = { accountKey, x25519, rank, seed ->
                        val envelope = uniffi.lakko_ffi_uniffi.composeRegistrarSeedEnvelope(
                            record.groupId,
                            accountKey,
                            x25519,
                            me.lakko.app.ds.FfiDsStatementSigning.toFfiRank(rank),
                            seed,
                        )
                        broadcastControlToGroup(identity, groupIdHex, record.group, envelope)
                    },
                    devicesOf = devicesOf,
                )
                result.sentSet?.let { sentRegistrarSets[groupIdHex] = it }
                store.put(accountIdB64, groupIdHex, result.delivered)
                if (result.complete) appointmentAttemptAtMs.remove(groupIdHex) else appointmentAttemptAtMs[groupIdHex] = now
                android.util.Log.i(
                    "ChatRepository",
                    "ADR-0057: registrar set converged (set sent=${result.sentSet != null}, " +
                        "${result.delivered.size} seeds held, complete=${result.complete})",
                )
            } finally {
                rootSeed.fill(0)
            }
        }
    }

    /**
     * Finding 4 — `true` when this account is known to hold [record]'s journal
     * root. Without the marker, probe (at most every [ROOT_PROBE_RETRY_MS]):
     * a `root` statement naming this account's own root, signed by it; `204`
     * sets the marker. An owner whose root is not confirmed appoints nothing.
     */
    private suspend fun probeRootIfUnheld(groupIdHex: String, record: GroupRecord): Boolean {
        if (isRootHeld(groupIdHex)) return true
        val wait = rootProbeNotBeforeMs[groupIdHex]
        if (wait != null && System.currentTimeMillis() < wait) return false
        rootProbeNotBeforeMs[groupIdHex] = System.currentTimeMillis() + ROOT_PROBE_RETRY_MS
        val root = rootStatementSigner(record.groupId) ?: return false
        return try {
            val held = me.lakko.app.ds.probeRoot(record.groupId, root) { make -> dsClient.putStatement(make) }
            if (held) {
                markRootHeld(groupIdHex)
                android.util.Log.i("ChatRepository", "ADR-0057: root probe answered 204; this account holds the root")
            } else {
                android.util.Log.i("ChatRepository", "ADR-0057: root probe answered 401; still using the registrar seed")
            }
            held
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.i("ChatRepository", "ADR-0057: root probe failed; retried later")
            false
        }
    }

    /** [reconcileAppointments] launched on the push scope, best effort. */
    private fun reconcileAppointmentsBestEffort(groupIdHex: String, force: Boolean = false) {
        pushScope.launch {
            try {
                reconcileAppointments(groupIdHex, force)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0057: appointment reconcile failed", cause)
            }
        }
    }

    /**
     * ADR-0057 — a member's account X25519 key, from its identity bundle. The
     * account id comes from the profile the member published in the group
     * (ADR-0048 v2) or the accepted-contact store; the fetched bundle must
     * name the SAME account key the MLS roster authenticated, else nothing is
     * sealed to it. `null` when either lookup is unavailable.
     */
    private suspend fun memberX25519KeyOrNull(accountKey: ByteArray): ByteArray? {
        val keyB64 = java.util.Base64.getEncoder().encodeToString(accountKey)
        val accountIdHex = profileDirectory.accountIdsByKey()[keyB64]
            ?: acceptedContactAccountIdHex(accountKey)
            ?: return null
        val accountId = hexToBytes(accountIdHex) ?: return null
        val bundle = try {
            discovery.identityFetch(accountId)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            null
        } ?: return null
        val served = try {
            me.lakko.app.auth.base64Decode(bundle.signature_pubkey)
        } catch (_: Throwable) {
            return null
        }
        if (!served.contentEquals(accountKey)) {
            android.util.Log.w("ChatRepository", "ADR-0057: identity bundle names another key; not appointing")
            return null
        }
        return try {
            me.lakko.app.auth.base64Decode(bundle.x25519_pubkey).takeIf { it.size == 32 }
        } catch (_: Throwable) {
            null
        }
    }

    /** The account id hex of an accepted contact whose key is [accountKey]. */
    private fun acceptedContactAccountIdHex(accountKey: ByteArray): String? =
        try {
            me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext)
                .firstOrNull { it.request.fromPubkey.contentEquals(accountKey) }
                ?.request?.fromAccountId
                ?.let { DsClient.pseudonymHex(it) }
        } catch (_: Throwable) {
            null
        }

    /**
     * ADR-0057 obligation 4 — the receive gate for a registrar-seed envelope.
     * Called from the poll's (or the sweep verdict's) locked apply region, so
     * the role table read is the live one at receipt.
     *
     *   * rank member / admin: kept only when it is addressed to this account
     *     AND its MLS sender resolved (C-68) to the group's owner; then opened
     *     with the ACCOUNT identity and stored, replacing any earlier seed.
     *   * rank root (ADR-0057 S2 finding 1, a root handover after an ownership
     *     transfer): kept only when it names this journal, is addressed to this
     *     account AND the live role table names this account owner; no sender
     *     check (the DS refuses a seed that is not the current root). The
     *     opened old root seed is handed to a DETACHED takeover (this region
     *     holds the group and provider locks the takeover's reconcile needs)
     *     and never enters the registrar-seed store.
     *
     * Never a timeline row, never logged.
     */
    private fun receiveRegistrarSeed(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
        envelope: ByteArray,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val decoded = try {
            uniffi.lakko_ffi_uniffi.decodeRegistrarSeedEnvelope(envelope)
        } catch (_: Throwable) {
            return
        }
        val senderBytes = senderKey?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        val groupId = try {
            group.groupId()
        } catch (_: Throwable) {
            return
        }
        val header = me.lakko.app.ds.SeedEnvelopeHeader(
            journalId = decoded.journalId,
            addresseeAccountKey = decoded.memberAccountKey,
            rank = me.lakko.app.ds.FfiDsStatementSigning.fromFfiSeedRank(decoded.rank),
            registrarPublicKey = decoded.registrarPublicKey,
        )
        val ownerAccountKey = roleTableOrNull(group)?.owner
        // ADR-0057 "Who revokes" item 1 — every member lists the appointment
        // (addressee -> registrar public key + rank) from an owner-sent
        // envelope, whoever it is addressed to, so an admin that later commits
        // that person's removal can revoke the key in the same registration.
        if (me.lakko.app.ds.registrarListEntryAccepted(header, groupId, senderBytes, ownerAccountKey)) {
            val rank = me.lakko.app.ds.journalRankOf(header.rank)
            if (rank != null) {
                try {
                    me.lakko.app.ds.recordListedRegistrar(
                        store = journalOwnershipStore.registrarList,
                        accountIdB64 = accountIdB64,
                        groupIdHex = groupIdHex,
                        addresseeAccountKey = header.addresseeAccountKey,
                        registrarPublicKey = header.registrarPublicKey,
                        rank = rank,
                    )
                } catch (_: Throwable) {
                    // Best effort: a missing entry costs the revocation until
                    // the owner's next registrars-set.
                }
            }
        }
        // ADR-0057 "Who revokes" item 2 — EVERY device of the addressee
        // account that reads the envelope opens and stores the seed (it is
        // sealed to the account key all of them hold); nothing here depends on
        // which device of the account processed it first.
        me.lakko.app.ds.receiveSeedEnvelope(
            header = header,
            groupId = groupId,
            ownAccountKey = ownAccountKey(),
            senderAccountKey = senderBytes,
            ownerAccountKey = ownerAccountKey,
            open = {
                val account = accountIdentity.get() ?: return@receiveSeedEnvelope null
                uniffi.lakko_ffi_uniffi.openRegistrarSeedEnvelope(envelope, account)
            },
            storeRegistrarSeed = { seed ->
                journalOwnershipStore.put(accountIdB64, groupIdHex, seed)
                // A seed lifts a hold a missing seed imposed (section 2.5a
                // item 8): this device can register from now on.
                sweepBackoff.reset(groupIdHex)
            },
            takeOver = { oldRootSeed -> startRootTakeover(groupIdHex, oldRootSeed) },
            onDrop = { reason -> android.util.Log.i("ChatRepository", "ADR-0057: $reason") },
        )
    }

    // -----------------------------------------------------------------
    // ADR-0057 S2 finding 1 — the journal root moves with an ownership transfer
    // -----------------------------------------------------------------

    /** Groups whose root handover is running now; the value is the rerun flag
     *  a schedule during the run raises. */
    private val handoverInFlight =
        java.util.concurrent.ConcurrentHashMap<String, java.util.concurrent.atomic.AtomicBoolean>()

    /** When a handover whose new owner did not resolve (or whose send failed)
     *  may be attempted again, per group. */
    private val handoverNotBeforeMs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** In-memory mirror of the owed-handover record for [owedLoadedForAccount],
     *  so the poll tail does not decrypt the store on every poll. */
    private val owedHandoverGroups: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()
    private val owedLoadedForAccount = AtomicReference<String?>(null)

    /** The owed-handover groups of [accountIdB64] (loaded once per account). */
    private fun owedHandoversFor(accountIdB64: String): Set<String> {
        if (owedLoadedForAccount.get() != accountIdB64) {
            synchronized(owedHandoverGroups) {
                if (owedLoadedForAccount.get() != accountIdB64) {
                    owedHandoverGroups.clear()
                    owedHandoverGroups.addAll(
                        try {
                            journalOwnershipStore.owedHandovers.groups(accountIdB64)
                        } catch (_: Throwable) {
                            emptySet()
                        },
                    )
                    owedLoadedForAccount.set(accountIdB64)
                }
            }
        }
        return owedHandoverGroups
    }

    private fun markHandoverOwed(accountIdB64: String, groupIdHex: String) {
        owedHandoversFor(accountIdB64)
        owedHandoverGroups.add(groupIdHex)
        try {
            journalOwnershipStore.owedHandovers.add(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            // Best effort: a lost record costs this device's handover after a
            // restart, not another device's of the same account.
        }
    }

    private fun clearHandoverOwed(accountIdB64: String, groupIdHex: String) {
        owedHandoverGroups.remove(groupIdHex)
        handoverNotBeforeMs.remove(groupIdHex)
        try {
            journalOwnershipStore.owedHandovers.remove(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            // best effort
        }
    }

    /** Drop the in-memory handover state (session end). */
    private fun resetRootHandoverState() {
        handoverInFlight.clear()
        handoverNotBeforeMs.clear()
        handoverSentAtMs.clear()
        takeoverInFlight.clear()
        takeoverNotBeforeMs.clear()
        sentRegistrarSets.clear()
        rootProbeNotBeforeMs.clear()
        journalRepair.cancelAll()
        synchronized(owedHandoverGroups) {
            owedHandoverGroups.clear()
            owedLoadedForAccount.set(null)
        }
    }

    /**
     * Report the role-table owner read [before] and [after] one commit apply
     * or sweep merge. Called from inside the group / provider locks, so it only
     * records and schedules; it never awaits the network. When the owner moved
     * away from this account: record the handover as owed (it survives a
     * restart), forget this account's appointment record for the group (it no
     * longer appoints under its root; the new owner re-appoints everyone) and
     * schedule the send.
     */
    private fun noteOwnerTransition(groupIdHex: String, before: ByteArray?, after: ByteArray?) {
        val own = ownAccountKey()
        // Finding 4 — the marker describes the DS, which moves only at the
        // takeover: a transition to or from this account clears it (a new
        // owner signs with its registrar seed until its takeover or probe
        // confirms the root).
        if (own != null && !(before contentEquals after) &&
            (before?.contentEquals(own) == true || after?.contentEquals(own) == true)
        ) {
            clearRootHeld(groupIdHex)
            sentRegistrarSets.remove(groupIdHex)
        }
        me.lakko.app.ds.ownerMovedAwayFrom(own, before, after) ?: return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        handoverSentAtMs.remove(groupIdHex)
        markHandoverOwed(accountIdB64, groupIdHex)
        try {
            journalOwnershipStore.appointments.put(accountIdB64, groupIdHex, emptyMap())
        } catch (_: Throwable) {
            // best effort: the record is only read by an owner
        }
        appointmentAttemptAtMs.remove(groupIdHex)
        handoverNotBeforeMs.remove(groupIdHex)
        android.util.Log.i("ChatRepository", "ADR-0057: ownership moved away; root handover owed")
        scheduleRootHandover(groupIdHex)
    }

    /** Schedule the root handover for [groupIdHex] if it is owed, not already
     *  running and not waiting out a retry interval. Launched, never awaited. */
    private fun scheduleRootHandover(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        if (groupIdHex !in owedHandoversFor(accountIdB64)) return
        val wait = handoverNotBeforeMs[groupIdHex]
        if (wait != null && System.currentTimeMillis() < wait) return
        val rerun = java.util.concurrent.atomic.AtomicBoolean(false)
        handoverInFlight.putIfAbsent(groupIdHex, rerun)?.let { running ->
            running.set(true)
            return
        }
        pushScope.launch {
            try {
                do {
                    rerun.set(false)
                    rootHandoverOnce(accountIdB64, groupIdHex)
                } while (rerun.get() && currentAccountIdB64.get() == accountIdB64)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0057: root handover did not run", cause)
            } finally {
                handoverInFlight.remove(groupIdHex, rerun)
            }
        }
    }

    /**
     * One step of an owed root handover (sending side). Finding 4 — the owed
     * record is kept until this account's own probe answers 401 (the root
     * moved); while it answers 204 the envelope is re-sent, at most every
     * [me.lakko.app.ds.HANDOVER_RESEND_MS], on session start and on group
     * activity (the poll tail).
     */
    private suspend fun rootHandoverOnce(accountIdB64: String, groupIdHex: String) {
        if (groupIdHex !in owedHandoversFor(accountIdB64)) return
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        val active = providerLock.withLock { readIsActive { record.group.isActive() } }
        if (active == false) {
            // Evicted: there is no group left to post the seed into.
            clearHandoverOwed(accountIdB64, groupIdHex)
            return
        }
        val outcome = me.lakko.app.ds.runOwedHandover(
            probeOwnRoot = {
                val root = rootStatementSigner(record.groupId) ?: throw IllegalStateException("no root signer")
                me.lakko.app.ds.probeRoot(record.groupId, root) { make -> dsClient.putStatement(make) }
            },
            lastSentAtMs = { handoverSentAtMs[groupIdHex] },
            nowMs = System::currentTimeMillis,
            send = {
                me.lakko.app.ds.sendRootHandover(
                    ownAccountKey = { ownAccountKey() },
                    liveOwner = { providerLock.withLock { roleTableOrNull(record.group)?.owner } },
                    ownerX25519 = { accountKey -> memberX25519KeyOrNull(accountKey) },
                    rootSeed = {
                        val account = rootAccountIdentity() ?: throw IllegalStateException("no account identity")
                        val accountSeed = account.ed25519Seed()
                        try {
                            uniffi.lakko_ffi_uniffi.dsJournalRootSeed(accountSeed, record.groupId)
                        } finally {
                            accountSeed.fill(0)
                        }
                    },
                    compose = { newOwner, x25519, rootSeed ->
                        uniffi.lakko_ffi_uniffi.composeRootHandoverEnvelope(record.groupId, newOwner, x25519, rootSeed)
                    },
                    sendEnvelope = { envelope -> broadcastControlToGroup(identity, groupIdHex, record.group, envelope) },
                )
            },
            log = { line -> android.util.Log.i("ChatRepository", "ADR-0057: $line") },
        )
        if (currentAccountIdB64.get() != accountIdB64) return
        val now = System.currentTimeMillis()
        when (outcome) {
            me.lakko.app.ds.OwedHandoverOutcome.DONE, me.lakko.app.ds.OwedHandoverOutcome.CANCELLED -> {
                clearHandoverOwed(accountIdB64, groupIdHex)
                handoverSentAtMs.remove(groupIdHex)
            }
            me.lakko.app.ds.OwedHandoverOutcome.SENT -> {
                handoverSentAtMs[groupIdHex] = now
                handoverNotBeforeMs[groupIdHex] = now + me.lakko.app.ds.HANDOVER_RESEND_MS
            }
            me.lakko.app.ds.OwedHandoverOutcome.THROTTLED -> {
                handoverNotBeforeMs[groupIdHex] =
                    (handoverSentAtMs[groupIdHex] ?: now) + me.lakko.app.ds.HANDOVER_RESEND_MS
            }
            me.lakko.app.ds.OwedHandoverOutcome.RETRY -> {
                handoverNotBeforeMs[groupIdHex] = now + APPOINTMENT_RETRY_INTERVAL_MS
            }
        }
        android.util.Log.i("ChatRepository", "ADR-0057: root handover $outcome")
    }

    /**
     * Receiving side (finding 4): keep the handed-over OLD root seed wrapped
     * under the device key ([me.lakko.app.ds.PendingRootSeedStore]) until the
     * takeover lands or answers 401, then start it detached from the locked
     * receive region. The in-memory copy is wiped here; the takeover reads its
     * own copy back from the store, so a network failure or a restart retries
     * it (session start and the poll tail call [scheduleTakeover]).
     */
    private fun startRootTakeover(groupIdHex: String, oldRootSeed: ByteArray) {
        val accountIdB64 = currentAccountIdB64.get()
        try {
            if (accountIdB64 == null) return
            try {
                journalOwnershipStore.pendingRootSeeds.put(accountIdB64, groupIdHex, oldRootSeed)
            } catch (cause: Throwable) {
                // Kept in memory for this attempt all the same.
                android.util.Log.w("ChatRepository", "ADR-0057: pending root seed not persisted", cause)
                val copy = oldRootSeed.copyOf()
                takeoverNotBeforeMs.remove(groupIdHex)
                launchTakeover(accountIdB64, groupIdHex, copy)
                return
            }
        } finally {
            oldRootSeed.fill(0)
        }
        takeoverNotBeforeMs.remove(groupIdHex)
        scheduleTakeover(groupIdHex)
    }

    /**
     * ADR-0066 section 3.5, "DS" - for every group journal this account
     * roots (it is the ADR-0022 owner), hand the root from the one the OLD
     * seed derives to the one the new seed derives: the old root seed is
     * kept as the pending takeover seed and the ADR-0057 takeover runs,
     * exactly as after an ownership handover (the `root` transfer signed by
     * the old root, then the registrar set replaced and the registrar seeds
     * re-sent). Durable: a takeover that does not land is retried at session
     * start and from the poll tail ([scheduleTakeover]).
     *
     * [predecessorSeed] and [currentSeed] are 64-byte phrase seeds (the
     * LATEST own predecessor and the session's current one). Returns how
     * many journals were scheduled. Throws when a pending seed could not be
     * persisted: the caller then keeps its step owed, because a takeover
     * that lives only in this process is not one a restart finishes.
     */
    public suspend fun transferOwnedJournalRoots(predecessorSeed: ByteArray, currentSeed: ByteArray): Int =
        withContext(mlsDispatcher) {
            val accountIdB64 = currentAccountIdB64.get()
                ?: throw IllegalStateException("ChatRepository not bootstrapped")
            // The new root derives from the identity the directory serves.
            refreshPublishedAccountIdentity(currentSeed)
            val owned = providerLock.withLock {
                groups.filter { (_, record) -> ownsJournal(record.group) }
                    .map { (groupIdHex, record) -> groupIdHex to record.groupId }
            }
            val oldAccountSeed = Identity.fromSeed(predecessorSeed).use { it.ed25519Seed() }
            try {
                for ((groupIdHex, groupId) in owned) {
                    val held = journalOwnershipStore.pendingRootSeeds.get(accountIdB64, groupIdHex)
                    if (held != null) {
                        // An earlier run (or an ownership handover) already
                        // keeps the root the journal is still under.
                        held.fill(0)
                    } else {
                        val rootSeed = uniffi.lakko_ffi_uniffi.dsJournalRootSeed(oldAccountSeed, groupId)
                        try {
                            journalOwnershipStore.pendingRootSeeds.put(accountIdB64, groupIdHex, rootSeed)
                        } finally {
                            rootSeed.fill(0)
                        }
                        // The store's write reports nothing: read it back.
                        val kept = journalOwnershipStore.pendingRootSeeds.get(accountIdB64, groupIdHex)
                            ?: throw IllegalStateException("the previous journal root was not kept")
                        kept.fill(0)
                    }
                }
            } finally {
                oldAccountSeed.fill(0)
            }
            for ((groupIdHex, _) in owned) {
                takeoverNotBeforeMs.remove(groupIdHex)
                scheduleTakeover(groupIdHex)
            }
            android.util.Log.i("ChatRepository", "ADR-0066: ${owned.size} owned journal root transfer(s) scheduled")
            owned.size
        }

    /** Groups whose takeover is running now. */
    private val takeoverInFlight: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** When a failed takeover may be attempted again, per group. */
    private val takeoverNotBeforeMs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** When this session last sent an owed root handover, per group. */
    private val handoverSentAtMs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** Run the takeover for [groupIdHex] if a pending seed is stored for it,
     *  none is running and no retry interval is pending. Launched. */
    private fun scheduleTakeover(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val wait = takeoverNotBeforeMs[groupIdHex]
        if (wait != null && System.currentTimeMillis() < wait) return
        val seed = try {
            journalOwnershipStore.pendingRootSeeds.get(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            null
        } ?: return
        launchTakeover(accountIdB64, groupIdHex, seed)
    }

    /** Launch one takeover with [seed] (owned here and wiped). The coroutine
     *  starts ATOMICALLY so the seed is always wiped. */
    private fun launchTakeover(accountIdB64: String, groupIdHex: String, seed: ByteArray) {
        if (!takeoverInFlight.add(groupIdHex)) {
            seed.fill(0)
            return
        }
        pushScope.launch(start = kotlinx.coroutines.CoroutineStart.ATOMIC) {
            try {
                val outcome = runRootTakeover(groupIdHex, seed) ?: return@launch
                if (currentAccountIdB64.get() != accountIdB64) return@launch
                if (outcome.dropsPendingSeed()) {
                    takeoverNotBeforeMs.remove(groupIdHex)
                    try {
                        journalOwnershipStore.pendingRootSeeds.remove(accountIdB64, groupIdHex)
                    } catch (_: Throwable) {
                        // A stale seed only earns another 401.
                    }
                } else {
                    takeoverNotBeforeMs[groupIdHex] = System.currentTimeMillis() + TAKEOVER_RETRY_MS
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                takeoverNotBeforeMs[groupIdHex] = System.currentTimeMillis() + TAKEOVER_RETRY_MS
                android.util.Log.w("ChatRepository", "ADR-0057: root takeover failed", cause)
            } finally {
                seed.fill(0)
                takeoverInFlight.remove(groupIdHex)
            }
        }
    }

    /** One takeover attempt; `null` when the group is not held here. Does not
     *  wipe [oldRootSeed] (the caller does). */
    private suspend fun runRootTakeover(groupIdHex: String, oldRootSeed: ByteArray): me.lakko.app.ds.TakeOverOutcome? {
        val record = groups[groupIdHex] ?: return null
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        val outcome = me.lakko.app.ds.takeOverJournal(
            journalId = record.groupId,
            oldRootSeed = oldRootSeed,
            ownRootPublicKey = { rootStatementSigner(record.groupId)?.publicKey },
            stillOwner = { providerLock.withLock { ownsJournal(record.group) } },
            oldRootTransfer = { seed, newRoot ->
                uniffi.lakko_ffi_uniffi.dsStatementSigner(seed).use { old ->
                    old.rootTransfer(record.groupId, newRoot).let { me.lakko.app.ds.DsStatement(it.path, it.bodyJson) }
                }
            },
            newRoot = { rootStatementSigner(record.groupId) },
            desiredRegistrars = {
                providerLock.withLock {
                    val table = roleTableOrNull(record.group) ?: return@withLock null
                    val roster = rosterLeavesOrNull(record.group) ?: return@withLock null
                    me.lakko.app.ds.desiredRegistrars(
                        memberAccounts = roster.map { it.accountKey },
                        ownerAccountKey = table.owner,
                        admins = table.admins,
                    )
                }
            },
            newRegistrarPublicKey = { accountKey ->
                val account = rootAccountIdentity() ?: throw IllegalStateException("no account identity")
                val accountSeed = account.ed25519Seed()
                val rootSeed = try {
                    uniffi.lakko_ffi_uniffi.dsJournalRootSeed(accountSeed, record.groupId)
                } finally {
                    accountSeed.fill(0)
                }
                try {
                    val registrarSeed = uniffi.lakko_ffi_uniffi.dsJournalRegistrarSeed(rootSeed, accountKey)
                    try {
                        uniffi.lakko_ffi_uniffi.dsStatementSigner(registrarSeed).use { it.publicKey() }
                    } finally {
                        registrarSeed.fill(0)
                    }
                } finally {
                    rootSeed.fill(0)
                }
            },
            probeOwnRoot = {
                val root = rootStatementSigner(record.groupId) ?: throw IllegalStateException("no root signer")
                me.lakko.app.ds.probeRoot(record.groupId, root) { make -> dsClient.putStatement(make) }
            },
            markRootHeld = { markRootHeld(groupIdHex) },
            put = { make -> dsClient.putStatement(make) },
            resetAppointments = {
                journalOwnershipStore.appointments.put(accountIdB64, groupIdHex, emptyMap())
                journalOwnershipStore.remove(accountIdB64, groupIdHex)
                appointmentAttemptAtMs.remove(groupIdHex)
                sentRegistrarSets.remove(groupIdHex)
            },
            reconcile = { reconcileAppointmentsBestEffort(groupIdHex, force = true) },
            log = { line -> android.util.Log.i("ChatRepository", "ADR-0057: $line") },
        )
        android.util.Log.i("ChatRepository", "ADR-0057: root takeover $outcome")
        return outcome
    }

    /**
     * ADR-0057 obligation 8 — bind [group]'s journal to [pushGid] with this
     * device's authority over the journal (the root, or during an unconfirmed
     * takeover the owner's registrar seed) when it owns the group, or its admin
     * registrar; a member never binds ([me.lakko.app.ds.PushBindingOutcome.NOT_PERMITTED],
     * nothing sent). Never throws; `409` / `503` / failures are classified and
     * logged. Since the second security pass (finding 5) the routing code binds
     * BEFORE it broadcasts the secret.
     */
    private suspend fun bindPushGidBestEffort(
        groupIdHex: String,
        group: Group,
        groupId: ByteArray,
        pushGid: ByteArray,
    ): me.lakko.app.ds.PushBindingOutcome {
        val signer = providerLock.withLock {
            val record = groups[groupIdHex]
            if (ownsJournal(group)) {
                if (record != null) authoritySignerFor(record, groupIdHex) else rootStatementSigner(groupId)
            } else {
                val own = ownAccountKey()
                val isAdmin = own != null &&
                    roleTableOrNull(group)?.admins?.any { it.contentEquals(own) } == true
                if (isAdmin) registrarStatementSigner(groupIdHex) else null
            }
        } ?: return me.lakko.app.ds.PushBindingOutcome.NOT_PERMITTED
        val error = try {
            dsClient.putStatement { signer.pushBinding(groupId, pushGid) }
            null
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            cause
        }
        val outcome = me.lakko.app.ds.pushBindingOutcomeOf(error)
        if (outcome != me.lakko.app.ds.PushBindingOutcome.BOUND) {
            android.util.Log.w("ChatRepository", "ADR-0057: push binding not taken: $outcome")
        }
        return outcome
    }

    /**
     * ADR-0053 client contract point 2 — the two mailboxes this session owns:
     * the ACCOUNT identity's (the stranger inbox contact requests arrive in)
     * and this DEVICE's (where Welcomes sealed to its KeyPackages arrive).
     * Installs their signers so every read is signed, and claims both. Called
     * at every bootstrap, i.e. after every signup, login and restore.
     */
    private fun claimOwnMailboxesBestEffort() {
        // ADR-0066 part 3: and the mailbox of the PUBLISHED account key when
        // it differs from the conversation identity's (after a rotation).
        //
        // ADR-0066 section 3.5a rule 4: the conversation identity's mailbox
        // is then a PREDECESSOR's: it is read (hence signed for) only for
        // 30 days after this device adopted the new key, and nothing is
        // registered there. Past the window it is left alone.
        val published = publishedAccountIdentity.get()
        val conversation = accountIdentity.get()?.takeIf { published == null || predecessorMailboxStillRead(it) }
        val identities = listOfNotNull(conversation, published, canonicalIdentity.get())
        for (identity in identities) {
            val (pseudonym, signer) = try {
                val ffi = uniffi.lakko_ffi_uniffi.dsMailboxSigner(identity)
                ffi.mailboxPseudonym() to FfiDsJournalSigner(ffi)
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0053: mailbox signer unavailable", cause)
                continue
            }
            dsSigners.installStatic(dsMailboxKey(pseudonym), signer, owner = DS_SIGNER_OWNER)
            pushScope.launch {
                try {
                    dsClient.claimMailbox(pseudonym, signer)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0053: mailbox claim did not land", cause)
                }
            }
        }
    }

    /** Whether the mailbox of [predecessor] (one of this account's previous
     *  identities) is inside its read window (ADR-0066 3.5a rule 4). An
     *  unreadable record reads: mail already sent must not be lost to it. */
    private fun predecessorMailboxStillRead(predecessor: Identity): Boolean {
        val accountIdB64 = currentAccountIdB64.get() ?: keyPackageAccount.get()?.accountIdB64 ?: return true
        return try {
            val key = predecessor.signaturePublicKey()
            val rows = me.lakko.app.auth.OwnPredecessorStore.onDevice(applicationContext).load(accountIdB64)
            try {
                val row = rows.firstOrNull { it.signaturePubkey.contentEquals(key) } ?: return true
                me.lakko.app.reach.predecessorMailboxIsRead(row.supersededAtMs, System.currentTimeMillis())
            } finally {
                rows.forEach { it.zero() }
            }
        } catch (_: Throwable) {
            true
        }
    }

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
        is uniffi.lakko_ffi_uniffi.ActingCommitter.Defer ->
            ActingCommitter.Defer(verdict.toRank, verdict.toSignatureKey)
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
     * ADR-0045 UX §7 — the [proposalKey] of every live pending proposal.
     *
     * Distinct from [pendingMembershipRequests], which folds the observation
     * store in and applies the UX §10 visibility gate: this is the raw set, and
     * the retry verdict needs it raw. A request the visibility gate hides from
     * this member is still a request that would collide with a re-send.
     *
     * Empty on any failure — the direction that suppresses a fire rather than
     * inventing one.
     */
    public fun pendingProposalKeys(groupIdHex: String): List<String> {
        val record = groups[groupIdHex] ?: return emptyList()
        return try {
            record.group.pendingProposals().map(::proposalKey)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "pendingProposalKeys failed", cause)
            emptyList()
        }
    }

    /** The signed-in account id, base64 — the first half of a per-`(account,
     *  group)` scope key. `null` before bootstrap. */
    public fun currentAccountIdB64(): String? = currentAccountIdB64.get()

    /**
     * ADR-0045 S7 residual 1 / ADR-0050 section 2.5a point 5 — the
     * `Not applied` rows the user has not dismissed: own requests whose
     * automatic re-request stopped ([reRequestSupersededAfterCommit]).
     */
    public fun notAppliedRequests(groupIdHex: String): List<UnappliedRequest> {
        val accountIdB64 = currentAccountIdB64.get() ?: return emptyList()
        return try {
            loadUnapplied(unappliedRequests, accountIdB64, groupIdHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "notAppliedRequests failed", cause)
            emptyList()
        }
    }

    /** Dismiss one `Not applied` row by [UnappliedRequest.rowKey]. It stays
     *  dismissed: a later stop of the same request must not bring it back. */
    public fun dismissNotApplied(groupIdHex: String, rowKey: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            dismissUnapplied(unappliedRequests, accountIdB64, groupIdHex, rowKey)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "dismissNotApplied failed", cause)
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
     * ADR-0050 section 2.5a point 1 — record what THIS device just asked for,
     * so a sweep that drops the request can be answered by re-requesting it
     * from the intent ([reRequestSupersededAfterCommit]). Called beside
     * [noteOwnRequestBestEffort] once the proposal is on the wire; an
     * unpublished request is not a request. Proposal path only: a table-less
     * group commits directly, and a direct commit cannot be superseded.
     */
    private fun noteOwnIntentBestEffort(groupIdHex: String, intent: RetryableRequest) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            recordOwnRequestIntent(
                store = pendingRequestObservations,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                intent = intent,
                atMs = System.currentTimeMillis(),
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "pending request: note intent failed", cause)
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
    /**
     * The one-to-one chat with the person holding [peerKey]: the id a contact
     * row linked at accept, but only while this device still holds that chat,
     * else the two-person chat whose other member holds the key, else `null`.
     *
     * A linked id whose chat is gone (deleted here, or never present on this
     * device) is NOT an address - seen live on web 2026-09-26 as a contact
     * whose Message opened "Conversation not found". The same trust existed
     * here and is closed the same way.
     */
    public fun chatGroupIdHexFor(linkedGroupIdHex: String?, peerKey: ByteArray): String? {
        if (linkedGroupIdHex != null && groups.containsKey(linkedGroupIdHex)) return linkedGroupIdHex
        return groups.keys.firstOrNull { hex -> oneToOnePeerKey(hex)?.contentEquals(peerKey) == true }
    }

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
            // The whole row, own request intents included: the prune keeps
            // those, and they describe a group this device is deleting.
            pendingRequestObservations.save(accountIdB64, groupIdHex, emptyMap())
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
        // ADR-0052 decision 3 — the pins are wiped with the conversation.
        try {
            PinStore.deleteGroup(applicationContext, accountIdB64, groupIdHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0052 delete chat: pins", cause)
        }
        // ADR-0059 section 6 — and the channel lists shared in it.
        sharedChannels.forgetGroup(groupIdHex)
        // ADR-0065 phase 2 — the closed state goes with the conversation.
        try {
            ConversationClosedStore.remove(applicationContext, accountIdB64, groupIdHex.lowercase())
            closedStates.remove(groupIdHex.lowercase())
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0065 delete chat: closed state", cause)
        }
        // LAST, and only after everything above had its chance: dropping the
        // registry row is what makes the chat unreachable, so it must not
        // happen while a transcript is still behind it.
        val removed = synchronized(groups) { groups.remove(groupIdHex) }
        dsSigners.remove(dsGroupKeyHex(groupIdHex))
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
            // ADR-0050 section 2.5a point 6 — an own request that left the
            // pending set is no longer judged here. This fold runs whenever the
            // panel reads, not on a commit, and the prune below keeps every own
            // request INTENT for [reRequestSupersededAfterCommit], which re-sends
            // a dropped request and lists it under `Not applied` only when the
            // re-request stops.
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
        // ADR-0050 section 2.5a item 6 — the DEVICE key the group runs on (the
        // identity sweeps use), never the account key: it is what breaks a
        // same-rank tie with a sibling device and what the core recognises as
        // this device's own echoed beacon.
        val ownSignatureKey = canonicalIdentity.get()?.signaturePublicKey()
            ?: return ActingCommitter.Defer(null)
        return try {
            providerLock.withLock {
                decideActingCommitter(
                    store = beaconObservations,
                    accountIdB64 = accountIdB64,
                    groupIdHex = groupIdHex,
                    ownRank = ownRank,
                    ownSignatureKey = ownSignatureKey,
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
                    fold = { rank, ownKey, observed, windowMs ->
                        // Fallible since item 6: a key that is not 32 bytes
                        // throws, which the catch below turns into a defer.
                        toActingCommitter(
                            uniffi.lakko_ffi_uniffi.actingCommitterVerdict(
                                rank,
                                ownKey,
                                observed.map {
                                    uniffi.lakko_ffi_uniffi.BeaconObservation(it.rank, it.ageMs, it.signatureKey)
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
    ): String =
        withContext(mlsDispatcher) { emitCommitterBeaconIfDueOnMlsThread(groupIdHex = groupIdHex, foreground = foreground) }

    private suspend fun emitCommitterBeaconIfDueOnMlsThread(
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
        var canRegister = false
        val ownRank = try {
            providerLock.withLock {
                if (!usesProposalPath(record.group)) return BeaconEmitVerdict.NOT_A_CANDIDATE.name
                // ADR-0050 section 2.5a item 8 — the sweep's own pre-stage
                // check: a device holding neither the journal root nor a
                // registrar seed never stages, so it must not beacon either.
                canRegister = commitStatementSignerFor(record, groupIdHex) != null
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
            canRegister = canRegister,
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
     * `ownCommitterRank()` — and, since ADR-0050 section 2.5a, a plain member
     * while it holds a device admission, ranked after the admins and sweeping
     * only those plus its own account's self-service proposals (everything
     * else in the set is dropped by the merge and re-requested by its author,
     * [reRequestSupersededAfterCommit]); which of them ACTS is decided by the
     * beacon fold.
     * Every device of a ranked account holds the account's rank (section 2.5a
     * item 6). The lowest rank whose device hears nothing better placed acts:
     * a device defers to a fresh beacon of a strictly lower rank, or of its
     * own rank with a lower signature key (a sibling device of its account),
     * so the acting device of an account is its lowest-keyed ACTIVE one. The
     * owner's devices therefore defer only to each other, and an admin acts
     * when nobody better placed has beaconed inside the freshness window,
     * which includes the case where it has heard nothing at all. So an
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
    public suspend fun sweepIfCommitter(groupIdHex: String): SweepOutcome? =
        withContext(mlsDispatcher) { sweepIfCommitterOnMlsThread(groupIdHex = groupIdHex) }

    private suspend fun sweepIfCommitterOnMlsThread(groupIdHex: String): SweepOutcome? {
        // UX 16.4 — a closed one-to-one commits nothing and reads nothing.
        if (closedOneToOne(groupIdHex)) return null
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
                MainThreadGuard.assertOffMain("sweepIfCommitter")
                var mayCommit = true
                // ADR-0057 S2 finding 1 — the owner before the sweep, so a
                // merged sweep (or a remote commit its verdict applied) that
                // carried this account's ownership transfer is seen.
                var ownerBeforeSweep: ByteArray? = null
                membersBefore = providerLock.withLock {
                    // ADR-0050 section 2.5a — a sweep run outside a poll (after
                    // a membership action) still counts as an observed commit
                    // for the superseded check at the next poll tail, which
                    // needs the epoch it started from.
                    runCatching { record.group.epoch() }.getOrNull()?.let {
                        supersededCheckedEpoch.putIfAbsent(groupIdHex, it)
                    }
                    ownerBeforeSweep = roleTableOrNull(record.group)?.owner
                    // ADR-0057 obligation 5 — a device holding neither the root
                    // nor a registrar key cannot register the epoch a sweep
                    // reaches, so it does not stage one; the sweep is left to
                    // an admin or the owner. A verdict ALREADY owed (a staged
                    // sweep from before) is still adjudicated below.
                    mayCommit = commitStatementSignerFor(record, groupIdHex) != null ||
                        runCatching { record.group.hasStagedSweep() }.getOrDefault(false)
                    memberSignatureKeysOrNull(record.group)
                }
                if (!mayCommit) return null
                try {
                    runCommitterSweep(sweepPortsFor(record, identity, groupIdHex))
                } finally {
                    val ownerAfterSweep = try {
                        withContext(kotlinx.coroutines.NonCancellable) {
                            providerLock.withLock { roleTableOrNull(record.group)?.owner }
                        }
                    } catch (_: Throwable) {
                        null
                    }
                    noteOwnerTransition(groupIdHex, ownerBeforeSweep, ownerAfterSweep)
                }
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
        // ADR-0057 obligation 3 — the sweep landed a membership or role change:
        // the owner appoints the newcomers, re-ranks, and drops the departed.
        reconcileAppointmentsBestEffort(groupIdHex, force = true)
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
        val membersAfterSweep = providerLock.withLock { memberSignatureKeysOrNull(record.group) }
        val evictedBySweep = readIsActive { record.group.isActive() } == false
        rotatePushRoutingIfShrankBestEffort(
            groupIdHex = groupIdHex,
            group = record.group,
            identity = identity,
            previousMemberKeys = membersBefore,
            currentMemberKeys = membersAfterSweep,
            selfEvicted = evictedBySweep,
        )
        // ADR-0048 §2 — a swept Add brings in a newcomer who needs this
        // device's profile too.
        republishProfileIfGrown(
            groupIdHex = groupIdHex,
            previousMemberKeys = membersBefore,
            currentMemberKeys = membersAfterSweep,
            selfEvicted = evictedBySweep,
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
                    // ADR-0057 — the leaves the commit takes out: a Remove's
                    // target, a Leave's own proposer.
                    val removed = staged.swept.mapNotNull {
                        when (val kind = it.kind) {
                            is uniffi.lakko_ffi_uniffi.ProposalKind.Remove -> kind.targetLeaf
                            is uniffi.lakko_ffi_uniffi.ProposalKind.Leave -> it.senderLeaf
                            else -> null
                        }
                    }
                    return StagedSweepArtifacts(
                        commit = staged.commit,
                        welcome = staged.welcome,
                        addedSignatureKeys = added,
                        removedLeaves = removed,
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
                        val sweptEvent = toEvent(
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
                            resolveLeafSignatureKey = { leaf ->
                                resolveLeafSignatureKey(rosterOf(record.group), leaf)
                            },
                        )
                        record.consumeEvents(
                            events = listOf(sweptEvent),
                            callSignalingSink = { messageId, payload, createdAt, senderLeaf, senderKey, senderPersonKey ->
                                routeIncomingCallSignaling(
                                    groupIdHex, messageId, payload, createdAt, senderLeaf, senderKey, senderPersonKey,
                                )
                            },
                            profileSink = { senderKey, profile ->
                                receiveProfileBestEffort(senderKey, profile)
                                receiveProfileGrantsBestEffort(record.group, groupIdHex, senderKey, profile)
                            },
                            scopedSink = { senderKey, kind, bytes ->
                                receiveScopedEnvelopeBestEffort(record.group, groupIdHex, senderKey, kind, bytes)
                            },
                            groupNameSink = { senderKey, name ->
                                receiveGroupName(record.group, groupIdHex, senderKey, name)
                            },
                            groupClosedSink = { senderKey, closed ->
                                receiveGroupClosed(record.group, groupIdHex, senderKey, closed)
                            },
                            inert = closedOneToOne(groupIdHex),
                            pinSink = { senderKey, op ->
                                receivePin(record.group, groupIdHex, senderKey, op)
                            },
                            sharedChannelsSink = { senderKey, list ->
                                sharedChannels.receive(groupIdHex, senderKey, list)
                            },
                            registrarSeedSink = { senderKey, envelope ->
                                receiveRegistrarSeed(record.group, groupIdHex, senderKey, envelope)
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
                            profileRequestSink = { senderKey ->
                                answerProfileRequestBestEffort(record.group, groupIdHex, senderKey)
                            },
                        )
                        persistReadWatermarksIfReceipts(groupIdHex, record, listOf(sweptEvent))
                        // A swept chat message counts toward the chat list's
                        // unread badge exactly as a polled one does.
                        localUnread.onIncomingApplied(
                            groupIdHex,
                            if (sweptEvent is IncomingChatEvent.Application) 1 else 0,
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

                // ADR-0050 section 2.5a item 7 — never unsigned: with no
                // grant accepted the Welcome stays owed.
                override suspend fun publishMailbox(pseudonym: ByteArray, bytes: ByteArray): Long =
                    publishSweepWelcome(pseudonym, bytes, groupIdHex)
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
                                    sweepWelcomeLeafKeysB64 = entry.welcomeLeafKeysB64,
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

                override suspend fun findAllOwedWelcomes(): List<SweepDurableEntry> =
                    try {
                        CommitOutboxStore
                            .loadForGroup(applicationContext, accountIdB64, groupIdB64)
                            .filter { it.isSweep && !it.acked && it.sweepWelcomesOwed }
                            .map {
                                SweepDurableEntry(
                                    id = it.id,
                                    commitB64 = it.commitB64,
                                    welcomeEnvelopeB64 = it.secondArtifactB64,
                                    welcomeLeafKeysB64 = it.sweepWelcomeLeafKeysB64,
                                    commitMessageId = it.commitMessageId,
                                )
                            }
                    } catch (_: Throwable) {
                        emptyList()
                    }

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
                                    welcomeLeafKeysB64 = it.sweepWelcomeLeafKeysB64,
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
            // ADR-0060 §6 — one Welcome write per added ACCOUNT, into its
            // account mailbox: each added leaf resolved through its device
            // binding in the merged roster.
            welcomeTargets = { leafKeys ->
                val roster = try {
                    record.group.members()
                } catch (_: Throwable) {
                    emptyList()
                }
                welcomeAccounts(leafKeys) { leaf ->
                    roster.firstOrNull { it.signaturePublicKey.contentEquals(leaf) }?.accountKey
                }.map { account -> welcomeMailboxFor(account) }
            },
            wrapWelcome = { welcome -> wrapEnvelope(KIND_WELCOME, welcome) },
            markOwnPublished = { id -> chatClient.markOwnPublishedCommit(groupIdHex, id) },
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
            // ADR-0057 obligation 5 — register the staged epoch's key with this
            // account's root (owner) or registrar signer before the sweep is
            // recorded or published, revoking the registrar keys of the
            // accounts the sweep evicts (owner only). The signers are derived
            // under the provider lock; the PUT runs outside it (the sweep holds
            // only the per-group lock here, and nothing is eager-merged).
            registerStagedEpoch = { staged ->
                data class Prepared(
                    val next: DsJournalSigner?,
                    val current: DsJournalSigner?,
                    val statements: me.lakko.app.ds.DsStatementSigning?,
                    val revoke: List<ByteArray>,
                    val evictees: List<ByteArray>,
                    val rank: me.lakko.app.ds.JournalRepairRank,
                )
                // ADR-0057 — a claim of this journal launched on open may still
                // be in flight; register after it, not ahead of it (bounded).
                ownerClaimGate.awaitBeforeRegistering(groupIdHex)
                val prepared = providerLock.withLock {
                    val next = record.group.dsStagedAccessSigner(identity)?.let(::FfiDsJournalSigner)
                    val evictees = me.lakko.app.ds.evictedAccounts(
                        before = rosterLeavesOrNull(record.group).orEmpty(),
                        removedLeaves = staged.removedLeaves,
                    )
                    Prepared(
                        next = next,
                        current = currentJournalSigner(groupIdHex),
                        statements = commitStatementSignerFor(record, groupIdHex),
                        revoke = revokeKeysFor(record, groupIdHex, evictees),
                        evictees = evictees,
                        rank = journalRepairRankFor(record, groupIdHex),
                    )
                }
                // Append: a 409 abandons the staged sweep (the caller's
                // rollback), never a Replace; an owner or admin notes the
                // group for the repair check.
                try {
                    noteAppendConflict(groupIdHex, record.groupId, prepared.current, prepared.rank) {
                        me.lakko.app.ds.registerEpochBeforePublish(
                            journalId = record.groupId,
                            before = prepared.current,
                            after = prepared.next,
                            statements = prepared.statements,
                            revoke = prepared.revoke,
                            put = { make -> dsClient.putStatement(make) },
                        )
                    }
                } catch (cause: Throwable) {
                    // A refusal that will not change on its own holds the next
                    // stage of this group; the sweep itself is abandoned by the
                    // caller exactly as before. Since ADR-0050 section 2.5a
                    // item 8 that includes a registration this device could
                    // not even sign (`DsRegistrationUnavailable`: no root or
                    // registrar seed, or no access signer): re-staging every
                    // poll would only abandon again.
                    if (SweepRegistrationBackoff.isBackoffRefusal(cause)) {
                        val hold = sweepBackoff.onRefused(groupIdHex, prepared.current?.epoch)
                        android.util.Log.i(
                            "ChatRepository",
                            "ADR-0057: sweep registration refused (${cause.javaClass.simpleName}); " +
                                "next attempt in ${hold.delayMs} ms",
                        )
                    }
                    throw cause
                }
                sweepBackoff.reset(groupIdHex)
                forgetRevokedAppointments(groupIdHex, prepared.evictees, prepared.revoke)
            },
            mayStage = {
                val epoch = try {
                    record.group.epoch()
                } catch (_: Throwable) {
                    null
                }
                sweepBackoff.mayStage(groupIdHex, epoch)
            },
        )
    }

    public suspend fun sweepPendingCommits(groupIdHex: String): Unit =
        withContext(mlsDispatcher) { sweepPendingCommitsOnMlsThread(groupIdHex = groupIdHex) }

    private suspend fun sweepPendingCommitsOnMlsThread(groupIdHex: String): Unit {
        // UX 16.4 — a closed one-to-one resolves nothing it owed: the verdict
        // read is a journal fetch, and the owed commit / Welcome re-publish is
        // a send. A sweep left staged only freezes this group's own membership
        // operations, which a closed one-to-one never runs again.
        if (!conversationActivity(conversationClosed(groupIdHex)).resolvesOwedCommits) return
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
        // ADR-0060 §6 — the account each owed Welcome is addressed to, so the
        // re-publish picks that person's reach grant.
        for (entry in entries) {
            val mailbox = entry.secondArtifactMailboxB64 ?: continue
            val account = entry.secondArtifactAccountKeyB64 ?: continue
            try {
                // ADR-0066 3.5a — the recorded key is what the leaf names;
                // the grant is picked for the person's current key.
                welcomeTargetAccounts[DsClient.pseudonymHex(java.util.Base64.getDecoder().decode(mailbox))] =
                    mailboxAccountKeyOf(java.util.Base64.getDecoder().decode(account))
            } catch (_: IllegalArgumentException) {
                // A malformed entry re-publishes unsigned.
            }
        }
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
                republishWelcome = { mailbox, bytes -> publishWelcomeToMailbox(mailbox, bytes, groupIdHex).id },
                markOwnPublished = { id -> chatClient.markOwnPublishedCommit(groupIdHex, id) },
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

    /**
     * ADR-0024 — after a batch that carried at least one read receipt, write
     * the record's watermark map to [readWatermarkStore].
     *
     * The snapshot is taken synchronously (it is an immutable map); the write
     * is LAUNCHED onto [pushScope] (the repository's process-lifetime
     * `Dispatchers.IO` scope), so it never runs on the main thread and never
     * while the caller's per-group or provider lock is held (G-37). The store's
     * merge is monotone, so launches landing out of order still converge. The
     * account is captured now and re-checked before writing, so a write that
     * outlives a logout / forget-device does not resurrect wiped rows.
     *
     * Call AFTER [GroupRecord.consumeEvents] has folded the events.
     */
    private fun persistReadWatermarksIfReceipts(
        groupIdHex: String,
        record: GroupRecord,
        events: List<IncomingChatEvent>,
    ) {
        if (events.none { it is IncomingChatEvent.ReadReceipt }) return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        pushScope.launch {
            val snapshot = record.readUpToSnapshot()
            if (snapshot.isEmpty()) return@launch
            if (currentAccountIdB64.get() != accountIdB64) return@launch
            try {
                readWatermarkStore.merge(accountIdB64, groupIdHex.lowercase(), snapshot)
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "read watermarks not persisted", cause)
            }
        }
    }

    /** ADR-0024 — seed one group's record from [readWatermarkStore]. Callers
     *  must not hold the provider or per-group lock (a preferences read). */
    private fun seedReadWatermarksBestEffort(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val record = groups[groupIdHex] ?: return
        try {
            val stored = readWatermarkStore.load(accountIdB64, groupIdHex.lowercase())
            if (stored.isNotEmpty()) record.seedReadWatermarks(stored)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "read watermarks not seeded", cause)
        }
    }

    /**
     * `docs/ux/calls.md` section 6 — a call in [groupIdHex] finished: write its
     * row into that conversation.
     *
     * Local and terminal: it appends one row, republishes the thread list and
     * persists. Nothing is composed, nothing is published to the DS journal, no
     * MLS state is touched — which is why it needs neither the provider lock nor
     * the per-group lock, only the timeline's own monitor
     * ([GroupRecord.appendCallRow]).
     *
     * The append and the republish are synchronous, so the row is on screen the
     * moment the call screen closes; the Keystore unwrap and the whole-transcript
     * write go to [callRowScope], because the caller is the call host on the main
     * dispatcher and a call teardown must not block a frame on disk.
     *
     * A group this device does not know is a no-op rather than an error: a call
     * can outlive leaving the chat it was placed from, and losing the row is a
     * better outcome than failing a call teardown.
     */
    public fun recordCallRow(groupIdHex: String, entry: me.lakko.app.calls.CallTranscriptEntry) {
        val record = groups[groupIdHex] ?: return
        val now = java.time.Instant.now()
        record.appendCallRow(entry, createdAt = now.toString())
        publishThreadList()
        // docs/ux/navigation.md section 6.2 — the Calls screen reads an index
        // written when the row is, never a walk of every transcript. Written on
        // the same dispatcher as the transcript persist and after it, so a
        // failure here can never cost the conversation its row.
        callRowScope.launch {
            persistMessageLogBestEffort()
            recordCallHistoryEntry(groupIdHex, entry, now.toEpochMilli())
        }
    }

    /**
     * File one finished call in the cross-conversation index.
     *
     * The kind comes from the group itself ([conversationKind], ADR-0062) —
     * the same read the conversation list routes on — so a Calls row opens
     * the screen its conversation opens and never a different one.
     */
    private fun recordCallHistoryEntry(
        groupIdHex: String,
        entry: me.lakko.app.calls.CallTranscriptEntry,
        startedAtMs: Long,
    ) {
        try {
            me.lakko.app.calls.CallHistoryStore.record(
                context = applicationContext,
                accountIdB64 = currentAccountIdB64.get(),
                entry = me.lakko.app.calls.CallHistoryEntry(
                    conversationIdHex = groupIdHex.lowercase(),
                    kind = conversationKind(groupIdHex),
                    outcome = entry.outcome,
                    video = entry.video,
                    startedAtMs = startedAtMs,
                    durationSecs = entry.durationSecs,
                ),
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "filing the call history entry failed", cause)
        }
    }

    /**
     * Fill the cross-conversation call index from calls that happened before
     * it existed. Once per account.
     *
     * Found live rather than by a test: the index only ever received calls
     * placed after it shipped, so on a device that had been calling for
     * months the Calls screen opened empty while those same calls were
     * visibly sitting in the conversation list. A screen that contradicts the
     * one next to it reads as broken, whatever the honest reason.
     *
     * It reads what is already in memory - every transcript row carries the
     * call's PARTS, not just its sentence, so nothing is re-parsed out of
     * words - and hands them to [me.lakko.app.calls.CallHistoryStore], which
     * runs at most once per account and never again.
     */
    public fun backfillCallHistory() {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        callRowScope.launch {
            me.lakko.app.calls.CallHistoryStore.backfillOnce(
                context = applicationContext,
                accountIdB64 = accountIdB64,
            ) { pastCallEntries() }
        }
    }

    /**
     * Every finished call this device still holds a transcript row for,
     * oldest first so the fold that files them keeps the newest.
     */
    private fun pastCallEntries(): List<me.lakko.app.calls.CallHistoryEntry> {
        val snapshot: List<Pair<String, List<ChatMessage>>> = synchronized(groups) {
            groups.map { (hex, record) -> hex to record.snapshot() }
        }
        val found = mutableListOf<me.lakko.app.calls.CallHistoryEntry>()
        for ((hex, messages) in snapshot) {
            val kind = conversationKind(hex)
            for (message in messages) {
                val call = message.call ?: continue
                // A row whose time cannot be read has no place in a list whose
                // whole order is time, so it is left where it already is.
                val at = me.lakko.app.ui.chat.parseMessageTimeMs(message.createdAt) ?: continue
                found += me.lakko.app.calls.CallHistoryEntry(
                    conversationIdHex = hex.lowercase(),
                    kind = kind,
                    outcome = call.outcome,
                    video = call.video,
                    startedAtMs = at,
                    durationSecs = call.durationSecs,
                )
            }
        }
        return found.sortedBy { it.startedAtMs }
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
                    messages = persistableMessages(record.snapshot()),
                    feedSeen = record.feedSeen.sorted(),
                    feedSeenThrough = record.feedSeenThrough,
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
        val threads = records.map { record ->
            val thread = record.toThread()
            val unread = localUnread.snapshot(thread.groupIdHex)
            thread.copy(
                localUnread = unread.count,
                unreadViewedThisSession = unread.viewedThisSession,
                // ADR-0065 phase 2 — the closed state (16.4 / 16.5), if any.
                closed = conversationClosed(thread.groupIdHex),
            )
        }
        mutableState.update { it.copy(threads = threads) }
        // ADR-0060 §1 — every group this account is still in carries a
        // `member` key, and the setting decides its `group` key.
        if (mutableState.value.bootstrapped) {
            me.lakko.app.reach.ReachManager.peek()?.setJoinedGroups(
                threads.filterNot { it.membershipEnded }.map { it.groupIdHex }.toSet(),
            )
        }
    }

    /**
     * The chat for [groupIdHex] is (or stops being) on screen. While it is,
     * incoming messages do not count toward its unread badge, and showing it
     * clears the count. Driven by the chat screen's lifecycle.
     */
    public fun setThreadOnScreen(groupIdHex: String, onScreen: Boolean) {
        localUnread.setOnScreen(groupIdHex, onScreen)
        publishThreadList()
    }

    /**
     * ADR-0063 section 5 — file what the feed kept on screen: [seen] maps a
     * group id hex to the message ids shown. Each group's seen set grows, its
     * watermark moves as far as the set is contiguous
     * ([me.lakko.app.feed.markSeen], the core's `contiguousSeen`), and the log
     * is persisted. Returns, per group whose watermark moved, the new
     * watermark — the caller sends the read receipt and the read cursor there.
     *
     * [readCursorThrough] is the cross-device read cursor of a group, which
     * counts as seen too; the larger of it and the feed's own watermark wins.
     * Runs under each group's lock so a poll applying the same group cannot
     * interleave with the read-modify-write.
     */
    public suspend fun recordFeedSeen(
        seen: Map<String, Collection<Long>>,
        readCursorThrough: (String) -> Long?,
        contiguous: (List<Long>, List<Long>) -> Long?,
    ): Map<String, Long> {
        val advanced = HashMap<String, Long>()
        var changed = false
        for ((hex, ids) in seen) {
            val record = groups[hex] ?: continue
            groupLock(hex).withLock {
                val rows = record.snapshot().map {
                    me.lakko.app.feed.SeenRow(
                        messageId = it.messageId,
                        direction = when (it.direction) {
                            Direction.Incoming -> "incoming"
                            Direction.Outgoing -> "outgoing"
                            Direction.System -> "system"
                        },
                    )
                }
                val cursor = readCursorThrough(hex)
                val own = record.feedSeenThrough
                val through = listOfNotNull(cursor, own).maxOrNull()
                val result = me.lakko.app.feed.markSeen(
                    rows = rows,
                    state = me.lakko.app.feed.GroupSeenState(through, record.feedSeen),
                    ids = ids,
                    contiguous = contiguous,
                )
                record.seedFeedSeen(result.state.feedSeen, result.state.through)
                if (result.newlySeen > 0) localUnread.onSeenElsewhere(hex, result.newlySeen)
                result.advancedTo?.let { advanced[hex] = it }
                changed = true
            }
        }
        if (changed) {
            persistMessageLogBestEffort()
            publishThreadList()
        }
        return advanced
    }

    private val screenPollLock = Any()
    private val screenPollCounts = HashMap<String, Int>()
    private val mutableScreenPolledGroups = MutableStateFlow<Set<String>>(emptySet())

    /**
     * Groups an open chat screen is long-polling itself. The app-level
     * all-groups loop leaves them out ([pollAllGroups]'s `exclude`) and
     * restarts when the set changes, so a group is never long-polled twice and
     * never left unpolled.
     */
    public val screenPolledGroups: StateFlow<Set<String>> = mutableScreenPolledGroups.asStateFlow()

    /** A chat screen started its own poll loop for [groupIdHex]. Pair with [releaseScreenPoll]. */
    public fun claimScreenPoll(groupIdHex: String) {
        synchronized(screenPollLock) {
            screenPollCounts[groupIdHex] = (screenPollCounts[groupIdHex] ?: 0) + 1
            mutableScreenPolledGroups.value = screenPollCounts.keys.toSet()
        }
    }

    /** The chat screen's poll loop for [groupIdHex] ended. */
    public fun releaseScreenPoll(groupIdHex: String) {
        synchronized(screenPollLock) {
            val remaining = (screenPollCounts[groupIdHex] ?: 0) - 1
            if (remaining > 0) screenPollCounts[groupIdHex] = remaining else screenPollCounts.remove(groupIdHex)
            mutableScreenPolledGroups.value = screenPollCounts.keys.toSet()
        }
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
    /** Incoming chat messages this device applied while the chat was not on
     *  screen, since it was last shown or since the process started (in
     *  memory). The chat list folds it with the cross-device read cursor. */
    val localUnread: Int = 0,
    /** The chat was on screen at some point in this process, so [localUnread]
     *  is an exact count rather than a lower bound. */
    val unreadViewedThisSession: Boolean = false,
    /** ADR-0063 section 5 — incoming ids the feed showed above the watermark;
     *  the unread count skips them. */
    val feedSeen: Set<Long> = emptySet(),
    /** ADR-0063 section 5 — the feed's own contiguous-seen watermark. */
    val feedSeenThrough: Long? = null,
    /** ADR-0065 phase 2 / UX 16.4 and 16.5 — the conversation is closed on
     *  this device (the peer of a one-to-one reset their keys; an admin closed
     *  the group after the owner's reset), with why and where to continue. */
    val closed: ConversationClosed? = null,
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
    /**
     * `docs/ux/calls.md` section 6 — set when this row is a finished call
     * rather than a message. Always a [Direction.System] row, so every existing
     * rule that treats system rows specially (they never join a visual run, they
     * carry no author, no op can target them) applies unchanged.
     *
     * Local to this device: written by the call host when a call ends, never
     * received, never sent. `null` for every other row.
     */
    val call: me.lakko.app.calls.CallTranscriptEntry? = null,
    /**
     * Whether this row reached the server.
     *
     * A message the user wrote appears in the conversation immediately and
     * settles afterwards, so the words are somewhere they can be seen and
     * retried rather than only in the composer. Before this the send was
     * awaited first and the row appended only on success: a message written
     * on a bad connection left the person looking at an error and an
     * unchanged composer, with nothing in the conversation at all.
     *
     * Defaults to [Ack.SENT] so every row that comes off the journal, the
     * message log or a call transcript is correct without naming it: those
     * rows exist BECAUSE they were delivered. Only the local send path sets
     * anything else.
     */
    val ack: Ack = Ack.SENT,
    /**
     * A client-side identity for a row this device created, `0` for a row
     * that came from the server.
     *
     * Needed because an unsent row has no [messageId] yet, and two of them
     * would then share the same lazy-list key - which means one row drawn for
     * two messages. It survives the flip to [Ack.SENT] so the row is not torn
     * down and rebuilt at the moment it lands, and it is never persisted: an
     * unsent row is not written at all.
     */
    val localId: Long = 0,
    /**
     * ADR-0052 — the message this one replies to, with the quote it carries.
     * `null` for an ordinary message. A reply is an ordinary chat message
     * that also points somewhere: [text] and [attachments] are its own, and
     * the quote is drawn from this alone, never from the target.
     */
    val reply: ReplyRef? = null,
    /**
     * ADR-0052 decision 2 — the attribution this message carries when it is a
     * forward: the original author's display name and, when the forwarder
     * could name it, where the original lives. `null` for an ordinary
     * message. The forwarder is still this row's author for everything else.
     */
    val forward: ForwardRef? = null,
    /**
     * ADR-0061 section 6 — the wire version of a chat envelope this build
     * cannot read, `null` for every readable row. [text] is empty for such a
     * row and every surface renders `chat.message.unsupported` instead, in
     * the interface language at display time.
     */
    val unsupportedVersion: Long? = null,
) {
    /**
     * The stable identity of this row.
     *
     * A row this device created prefers its [localId], and keeps preferring
     * it after the send lands: an identity that changed at that moment would
     * tear the row down and rebuild it mid-send.
     */
    val rowId: Long get() = if (localId != 0L) localId else messageId
}

/**
 * ADR-0052 decision 1 — a reply's reference to its target, as the envelope
 * carries it: the target's DS `messageId`, plus the quoted author's name and
 * an excerpt of the target's own text, both a SNAPSHOT taken when the reply
 * was composed. Either string may be absent (the core drops an empty one),
 * and the receiver then draws the fallback in `replyQuoteLines`. Neither is
 * verified: they are what the replier wrote, and the UI must not dress them
 * as confirmed.
 */
public data class ReplyRef(
    val target: Long,
    val author: String?,
    val excerpt: String?,
)

/**
 * ADR-0052 decision 2 — a forward's attribution, as the envelope carries it.
 *
 * [author] is the original author's display name as it stood when the message
 * was forwarded: a SNAPSHOT and a courtesy label, never a reference to a
 * person (`docs/ux/message-actions.md` section 5) — no username, no key, no
 * route. `null` when the forwarder carried none, and the bubble then reads
 * "Forwarded".
 *
 * [originGroupIdHex] + [originMessageId] point at the original MESSAGE, both
 * or neither, inside the encrypted payload only. They exist so a recipient who
 * is also in that conversation can jump to it; a recipient who is not sees
 * the name only, and nothing here says whether that conversation exists.
 */
public data class ForwardRef(
    val author: String?,
    val originGroupIdHex: String?,
    val originMessageId: Long?,
)

/** Whether an outgoing row reached the server. */
public enum class Ack {
    /** On its way. The row reads "Sending...". */
    PENDING,

    /** The server has it; [ChatMessage.messageId] and `createdAt` are real. */
    SENT,

    /**
     * It did not get there and nothing reached the other side. The row reads
     * "Not sent" and keeps the text, with a Retry beside it.
     */
    FAILED,
}

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
     * ADR-0027 / UX §7.1 — an external-join request arrived that the core did
     * NOT keep (the quarantine was full). It is absent from
     * [Group.pendingExternalJoins] and can be approved by nobody, so the
     * members sheet cannot learn about it from the retained set — this is the
     * only place the fact survives.
     *
     * Read by the sheet on its next refresh and cleared when an admin acts on
     * the queue, which is what may have made room.
     */
    var sawUnkeptExternalJoin: Boolean = false

    /** ADR-0053 — the knock queue's cursor (its own id space) and when it was
     *  last read. In memory: see `pollKnocksIfOwnerOrAdmin`. */
    @Volatile
    var knockCursor: Long = 0L

    @Volatile
    var lastKnockPollMs: Long = 0L

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
     *  higher one (re-sync replay is idempotent). Persisted by the
     *  repository into [ReadWatermarkStore] and seeded back via
     *  [seedReadWatermarks] after a relaunch.
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
     * batches. The loser of this CAS skips entirely (it does not even fetch),
     * or — the chat screen's loop — waits for the winner to finish; the winner
     * releases it in a `finally`, so a failed / cancelled fetch never wedges
     * the group. See [runSplitPoll] and [PollLatch].
     */
    val pollInFlight: PollLatch = PollLatch()

    /**
     * The next identity for a row this device creates.
     *
     * Negative and below every id this group already holds. DS ids are
     * positive and monotone, so a local row can never collide with one.
     * Allocated over [ChatMessage.rowId] rather than over `messageId`,
     * because identity is what must be unique and the two are not the same
     * field for a row that has not landed yet - a call row and an unsent
     * message both live down here.
     */
    fun nextLocalId(): Long = minOf(0L, messages.minOfOrNull { it.rowId } ?: 0L) - 1

    /**
     * Put a message the user just wrote into the conversation, before it has
     * been anywhere. It settles through [markSent] or [markFailed].
     */
    fun appendPending(
        localId: Long,
        text: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef>,
        createdAt: String,
        authorKey: MemberIdentity?,
        /** ADR-0052 — carried on the row so the sender sees its own quote. */
        reply: ReplyRef? = null,
        /** ADR-0052 decision 2 — carried on the row so the sender sees its
         *  own "Forwarded from" line, and so a retry sends it again. */
        forward: ForwardRef? = null,
    ) {
        messages.add(
            ChatMessage(
                messageId = 0,
                direction = Direction.Outgoing,
                text = text,
                attachments = attachments,
                createdAt = createdAt,
                authorKey = authorKey,
                ack = Ack.PENDING,
                localId = localId,
                reply = reply,
                forward = forward,
            ),
        )
    }

    /**
     * The send landed: take the server's id and time, and stop saying
     * "Sending...". The identity does not move, so the row is not rebuilt.
     */
    fun markSent(
        localId: Long,
        messageId: Long,
        createdAt: String,
        /**
         * ADR-0023 — our own IDENTITY, stamped here rather than at append
         * time because G-37 requires it to be read in the same provider
         * critical section as the ratchet advance: taken outside it, the
         * stamp can be read across another group's commit. A row that never
         * lands therefore carries no author, which is correct - it is not a
         * message anyone can edit or delete, and nothing can target it.
         */
        authorKey: MemberIdentity?,
    ) {
        val idx = messages.indexOfFirst { it.localId == localId }
        if (idx < 0) return
        messages[idx] = messages[idx].copy(
            messageId = messageId,
            createdAt = createdAt,
            authorKey = authorKey,
            ack = Ack.SENT,
        )
    }

    /** Trying again: back to "Sending...". */
    fun markPending(localId: Long) {
        val idx = messages.indexOfFirst { it.localId == localId }
        if (idx < 0) return
        messages[idx] = messages[idx].copy(ack = Ack.PENDING)
    }

    /**
     * The send did not get there. The row STAYS, with its text: that is the
     * whole point of appending it before the publish.
     */
    fun markFailed(localId: Long) {
        val idx = messages.indexOfFirst { it.localId == localId }
        if (idx < 0) return
        messages[idx] = messages[idx].copy(ack = Ack.FAILED)
    }

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
        callSignalingSink: ((Long, ByteArray, String, UInt?, MemberIdentity?, MemberIdentity?) -> Unit)? = null,
        pushRoutingSecretSink: ((ByteArray, ULong, UInt?) -> Unit)? = null,
        committerBeaconSink: ((UInt?, String) -> Unit)? = null,
        membershipProposalSink: ((ProposalRecord, String) -> Unit)? = null,
        profileSink: ((MemberIdentity?, uniffi.lakko_ffi_uniffi.FfiProfile) -> Unit)? = null,
        groupNameSink: ((MemberIdentity?, uniffi.lakko_ffi_uniffi.FfiGroupName) -> Unit)? = null,
        profileRequestSink: ((MemberIdentity?) -> Unit)? = null,
        pinSink: ((MemberIdentity?, uniffi.lakko_ffi_uniffi.FfiPinOp) -> Unit)? = null,
        sharedChannelsSink: ((MemberIdentity?, me.lakko.app.discover.SharedList) -> Unit)? = null,
        registrarSeedSink: ((MemberIdentity?, ByteArray) -> Unit)? = null,
        scopedSink: ((MemberIdentity?, me.lakko.app.reach.ScopedEnvelopeKind, ByteArray) -> Unit)? = null,
        groupClosedSink: ((MemberIdentity?, uniffi.lakko_ffi_uniffi.FfiGroupClosed) -> Unit)? = null,
        /** ADR-0065 phase 2 / UX 16.4 — the conversation is a closed
         *  one-to-one: EVERY row that arrives is dropped before it is stored,
         *  applied or routed ([dropArrivingRow]). The poll no longer fetches
         *  such a journal at all; this is the same rule for a batch that was
         *  already in flight when the chat closed. */
        inert: Boolean = false,
    ) {
        // ADR-0050 — a message this PERSON sent from another device arrives as an
        // incoming row (another leaf sealed it) but is ours: it renders on our
        // side, exactly as if this device had sent it.
        val ownPersonKey by lazy { ownIdentity(group) }
        for (event in events) {
            // UX 16.4 — a closed one-to-one applies nothing that arrives: no
            // late message (it would reopen the chat in the person's eyes), no
            // edit / delete / reaction over the read-only history, no call
            // frame, no typing or read state, no service envelope.
            if (dropArrivingRow(inert, arrivingRowOf(event))) continue
            when (event) {
                is IncomingChatEvent.Application -> messages.add(
                    ChatMessage(
                        messageId = event.messageId,
                        direction = if (event.senderKey != null && event.senderKey == ownPersonKey) {
                            Direction.Outgoing
                        } else {
                            Direction.Incoming
                        },
                        text = event.text,
                        attachments = event.attachments,
                        createdAt = event.createdAt,
                        // The author IDENTITY the classifier resolved at
                        // receipt, in the epoch that named the leaf. The leaf
                        // itself is deliberately not recorded: by the time this
                        // row is read back it may belong to whoever was added
                        // next.
                        authorKey = event.senderKey,
                        // ADR-0052 — the quote as the envelope carried it.
                        reply = event.reply,
                        // ADR-0052 decision 2 — the forward attribution as the
                        // envelope carried it; the name is never resolved.
                        forward = event.forward,
                        // ADR-0061 section 6 — a newer-version row, rendered
                        // from the catalog.
                        unsupportedVersion = event.unsupportedVersion,
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
                is IncomingChatEvent.ServiceEnvelope -> {
                    // An unrecognised or undecodable `lakko/` envelope (for
                    // example a newer profile version from another build). Not
                    // a timeline row and so never a chat-list preview either:
                    // dropped.
                }
                is IncomingChatEvent.CallSignaling -> {
                    // Phase 3a10 — route into the call state
                    // machines (the group-call ledger and the 1x1
                    // CallSession). Never touches the chat
                    // timeline. The DS row's `created_at` rides
                    // along because the ledger bounds a group
                    // call's announced start time by it.
                    callSignalingSink?.invoke(
                        event.messageId,
                        event.payloadBytes,
                        event.createdAt,
                        event.senderLeaf,
                        event.senderKey,
                        event.senderPersonKey,
                    )
                }
                is IncomingChatEvent.Profile -> {
                    // ADR-0048 — a member's profile. Not a timeline row. A
                    // malformed envelope (`profile == null`) is consumed as a
                    // no-op; the sink stores a well-formed one under the
                    // sender key the classifier resolved at receipt.
                    val profile = event.profile
                    if (profile != null) profileSink?.invoke(event.senderKey, profile)
                }
                is IncomingChatEvent.GroupName -> {
                    // ADR-0051 — the group's shared name. Not a timeline row. A
                    // malformed envelope (`groupName == null`) is consumed as a
                    // no-op; the sink runs the role gate + the stamp order and
                    // writes the store only on accept.
                    val name = event.groupName
                    if (name != null) groupNameSink?.invoke(event.senderKey, name)
                }
                is IncomingChatEvent.GroupClosed -> {
                    // ADR-0065 phase 2 / UX 16.5 — an admin closed the group.
                    // Not a timeline row of its own (the terminal row is the
                    // receiver's, drawn from the stored state); the sink runs
                    // the role gate + the stamp order and writes the store
                    // only on accept.
                    val closed = event.closed
                    if (closed != null) groupClosedSink?.invoke(event.senderKey, closed)
                }
                is IncomingChatEvent.Pin -> {
                    // ADR-0052 decision 3 — a shared pin / unpin. Not a
                    // timeline row. A malformed envelope (`pin == null`) is
                    // consumed as a no-op; the sink runs the standing gate +
                    // the per-message stamp order and writes the store only on
                    // accept.
                    val op = event.pin
                    if (op != null) pinSink?.invoke(event.senderKey, op)
                }
                is IncomingChatEvent.SharedChannels -> {
                    // ADR-0059 section 6 — a member's shared channel list. Not
                    // a timeline row and never persisted. A malformed envelope
                    // (`list == null`) is consumed as a no-op; the sink files a
                    // well-formed one under the sender key resolved at receipt.
                    val list = event.list
                    if (list != null) sharedChannelsSink?.invoke(event.senderKey, list)
                }
                is IncomingChatEvent.RegistrarSeed -> {
                    // ADR-0057 obligation 4 — a registrar seed. Not a timeline
                    // row; the sink runs the addressee + owner gate against the
                    // live role table and stores the seed only on accept.
                    registrarSeedSink?.invoke(event.senderKey, event.envelope)
                }
                is IncomingChatEvent.ScopedDiscovery -> {
                    // ADR-0060 — a scoped-discovery service envelope. Not a
                    // timeline row; the sink keeps what this device acts on
                    // and drops the rest.
                    scopedSink?.invoke(event.senderKey, event.kind, event.bytes)
                }
                is IncomingChatEvent.ProfileRequest -> {
                    // ADR-0048 trigger 4 — a member asking for everyone's
                    // profile. Not a timeline row. A malformed request
                    // (`wellFormed == false`) is consumed as a no-op — dropped,
                    // never answered; the sink answers a well-formed one with
                    // this device's own profile through the throttled growth
                    // path, unless the asker is this same person.
                    if (event.wellFormed) profileRequestSink?.invoke(event.senderKey)
                }
                is IncomingChatEvent.ExternalJoinRequest -> {
                    // ADR-0027 (G-3) — a non-member's external Add proposal,
                    // retained pending an owner/admin approval. Not a timeline
                    // row; the members sheet reads the retained set via
                    // ChatRepository.pendingExternalJoins(groupIdHex).
                    //
                    // Except for the one thing that read cannot see. A request
                    // the core did NOT keep is absent from the retained set, so
                    // it renders as nothing at all — correct about the Approve
                    // button and silent about the joiner, who is now waiting on
                    // an admin nobody told. §7.1 recorded that state as
                    // unsurfaceable; `retained` is what made it statable, so it
                    // is recorded here rather than dropped.
                    if (externalJoinAffordance(event.retained) is
                        ExternalJoinAffordance.Informational
                    ) {
                        sawUnkeptExternalJoin = true
                    }
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

    /** ADR-0024 — fold persisted watermarks into the live map. Monotone, so a
     *  receipt applied before the seed is never lowered by it. */
    fun seedReadWatermarks(stored: Map<MemberIdentity, Long>) {
        synchronized(presenceLock) {
            readUpToByMember = mergeReadWatermarks(readUpToByMember, stored)
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
        feedSeen = feedSeen,
        feedSeenThrough = feedSeenThrough,
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

    /** ADR-0063 section 5 — ids the feed kept on screen above the watermark. */
    @Volatile
    var feedSeen: Set<Long> = emptySet()
        private set

    /** ADR-0063 section 5 — the feed's own contiguous-seen watermark, beside
     *  the cross-device read cursor (which only exists with a DeviceGroup). */
    @Volatile
    var feedSeenThrough: Long? = null
        private set

    fun seedFeedSeen(ids: Set<Long>, through: Long?) {
        feedSeen = ids
        feedSeenThrough = through
    }

    /**
     * `docs/ux/calls.md` section 6 — append this device's own record of a
     * finished call to the end of the timeline.
     *
     * The row is local, so it needs an id no DS row will ever take. The DS
     * assigns strictly positive message ids, so local rows count **downwards
     * from zero**: a call row can therefore never collide with a journal row,
     * can never be the target of a peer's edit or delete op (nothing addresses a
     * negative id, and the row carries no author so the ADR-0023 gate would
     * refuse anyway), and stays unique across a relaunch because the counter is
     * re-derived from the lowest id already in the restored transcript.
     *
     * Appended rather than inserted by time: a call that has just ended is the
     * newest thing in the conversation, which is where the DS journal order puts
     * it too.
     */
    fun appendCallRow(entry: me.lakko.app.calls.CallTranscriptEntry, createdAt: String) {
        synchronized(messages) {
            val lowest = messages.minOfOrNull { it.messageId } ?: 0L
            messages.add(
                ChatMessage(
                    messageId = minOf(0L, lowest) - 1L,
                    direction = Direction.System,
                    text = me.lakko.app.calls.callRowText(entry),
                    createdAt = createdAt,
                    call = entry,
                ),
            )
        }
    }

    /**
     * ADR-0065 phase 2 / UX 16.3 — append a local system row ("{name} reset
     * their account keys. Their old devices were removed from this group.")
     * at the point the eviction landed. Local like a call row (a negative id
     * no DS row can take, no author), rendered in the interface language of
     * the moment it is written, as a call row is.
     */
    fun appendLocalSystemRow(text: me.lakko.app.l10n.UiText) {
        synchronized(messages) {
            val lowest = messages.minOfOrNull { it.messageId } ?: 0L
            messages.add(
                ChatMessage(
                    messageId = minOf(0L, lowest) - 1L,
                    direction = Direction.System,
                    text = text.render(),
                    createdAt = java.time.Instant.now().toString(),
                ),
            )
        }
    }

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
