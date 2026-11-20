// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import java.util.Collections
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.BindingEpoch
import me.lakko.app.contacts.ownMailboxIdentities
import me.lakko.app.contacts.sealFor
import me.lakko.app.launchBound
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import me.lakko.app.calls.toRow
import me.lakko.app.calls.withSender
import me.lakko.app.profile.toFfi
import me.lakko.app.push.GroupPushRouting
import me.lakko.app.push.NativePushRoutingPorts
import me.lakko.app.push.PushClient
import me.lakko.app.push.PushRouteStore
import me.lakko.app.push.PushRoutingPorts
import me.lakko.app.push.PushRoutingSecretStore
import me.lakko.app.auth.IdentitySecrets
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

/** ADR-0087 section 9 — pages of a journal read for a held Welcome before the
 *  pass gives up and tries again later. Chosen, not measured. */
private const val MAX_HELD_WELCOME_PAGES: Int = 50

/** ADR-0087 section 9 — the wait before a read another poll held. */
private const val HELD_WELCOME_RETRY_DELAY_MS: Long = 250L

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

/** ADR-0073 — how long the superseded-key read of the reset ledger is reused
 *  before the next send reads it again. An accepted reset invalidates it at once. */
private const val SUPERSEDED_CACHE_MS: Long = 15_000L

/** ADR-0073 R.6 — how often every held group is offered a continuity pass
 *  from the tail of a poll round. Chosen, not measured. */
private const val CONTINUITY_SWEEP_INTERVAL_MS: Long = 30_000L

/** ADR-0050 section 2.8 — how long [ChatRepository.admitDevice] waits for the
 *  new device's KeyPackage to appear in the directory, and how often it looks.
 *  The fresh device publishes it after it has landed a session. */
private const val ADMIT_DEVICE_WAIT_MS: Long = 180_000L
private const val ADMIT_DEVICE_POLL_MS: Long = 3_000L
/** ADR-0074 - how long `admitDevice` waits for a readable DeviceGroup roster. */
private const val OWN_ROSTER_WAITS: Int = 10
private const val OWN_ROSTER_WAIT_MS: Long = 500L

/** ADR-0053 — at most one knock-queue read per group in this window. */
// Raised from 10 s on 2026-11-05 to bring idle request volume down (chosen,
// not measured): a join request waits up to a minute for an admin's device.
private const val KNOCK_POLL_MIN_INTERVAL_MS: Long = 60_000L

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

    /**
     * The binding of this repository to the account it was bootstrapped
     * with. It ends on [clear] and when [bootstrap] is handed another
     * account. Every fire-and-forget coroutine below is launched with
     * [launchBound], so work started for one account stops at its next
     * suspension once that account is gone, instead of resuming into the
     * stores, the reach manager or the groups of the next one; [bootstrap]
     * captures it so a sign-out that lands while it restores wins.
     */
    private val sessionBinding = BindingEpoch()

    /** The binding current now (a host test reads when it ends). */
    internal fun captureSessionBinding(): me.lakko.app.Binding = sessionBinding.capture()

    /** [launchBound] on this repository's binding (a host test launches work
     *  the way the fire-and-forget paths do). */
    internal fun launchForSession(block: suspend CoroutineScope.() -> Unit): kotlinx.coroutines.Job =
        pushScope.launchSession(block = block)

    /** [forSession] around [block] (a host test calls in the way the
     *  suspending entry points do, without an MLS group). */
    internal suspend fun <T> callForSession(block: suspend () -> T): T = forSession { block() }

    /** [ensureSession], for the same test. */
    internal suspend fun ensureSessionForTest(): Unit = ensureSession()

    /**
     * Which binding the running call into this repository was started for.
     * Carried in the coroutine context, so the helpers a call goes through
     * can ask it ([ensureSession]) without every one of them taking it as a
     * parameter.
     */
    private class SessionWork(val binding: me.lakko.app.Binding) :
        kotlin.coroutines.AbstractCoroutineContextElement(Key) {
        companion object Key : kotlin.coroutines.CoroutineContext.Key<SessionWork>
    }

    /**
     * Run [block] on the repository's dispatcher as work of the account bound
     * NOW. Every suspending entry point a screen or a host calls goes through
     * here.
     *
     * The callers' coroutines are not this repository's to cancel, and they
     * outlive a sign-out more often than they look like they should (a call
     * host, a process-wide scope, a `catch (Throwable)` that swallowed the
     * cancellation). So the call itself asks: [ensureSession] after its
     * suspensions and on entering a lock region ([sessionLock]), and once more
     * here before anything is returned, so a caller never files what was read
     * for an account that has left. A call made from inside another keeps the
     * outer call's binding.
     */
    private suspend fun <T> forSession(block: suspend CoroutineScope.() -> T): T {
        val work = kotlinx.coroutines.currentCoroutineContext()[SessionWork]
            ?: SessionWork(sessionBinding.capture())
        work.binding.ensureCurrent()
        val result = try {
            withContext(mlsDispatcher + work, block)
        } catch (cause: Throwable) {
            // A call that fails after its session ended failed BECAUSE it
            // ended (closed handles, a cleared map): the caller is told that,
            // not shown an error about a request nobody is waiting for.
            work.binding.ensureCurrent()
            throw cause
        }
        work.binding.ensureCurrent()
        return result
    }

    /**
     * The account this call was started for is still the bound one, or the
     * call stops here ([me.lakko.app.StaleBindingException], a cancellation).
     *
     * Placed after a suspension and before whatever writes, publishes or acts
     * next. What it protects is everything that is NOT one of the handles the
     * call captured (those are closed with the session and throw): the
     * `groups` map and the per-account stores, which are read again at write
     * time and name the next account by then; the echo filter, the retry
     * records and the reach manager, which are keyed by group or bound to the
     * account; and two accounts on one phone that are members of the same
     * group, where a group id alone no longer says whose state it is. Deliberately
     * not a cancellation check: a caller that merely left the screen still
     * gets its sent message recorded.
     */
    private suspend fun ensureSession() {
        kotlinx.coroutines.currentCoroutineContext()[SessionWork]?.binding?.ensureCurrent()
    }

    /**
     * One network leg whose outcome somebody else files (a retry record, a
     * "distributed" flag): [ensureSession] on the way out, whichever way out
     * it is. A leg that FAILS after the session ended failed because the
     * session ended (its handles were closed under it), and reporting that as
     * a refusal would have the caller persist a verdict about a request that
     * was never judged.
     */
    private suspend inline fun <T> sessionLeg(leg: () -> T): T {
        val result = try {
            leg()
        } catch (cause: Throwable) {
            ensureSession()
            throw cause
        }
        ensureSession()
        return result
    }

    /** The binding of the running call, for the non-suspending callbacks it
     *  hands out (they cannot ask the coroutine context themselves). */
    private suspend fun callBinding(): me.lakko.app.Binding =
        kotlinx.coroutines.currentCoroutineContext()[SessionWork]?.binding ?: sessionBinding.capture()

    /** [Mutex.withLock], entered only by work of the account still bound: the
     *  wait for a lock is where a sign-out most often lands. */
    private suspend inline fun <T> Mutex.sessionLock(action: () -> T): T =
        withLock {
            ensureSession()
            action()
        }

    /**
     * [launchBound] on this repository's binding, with the binding in the
     * context as well: a launched coroutine is cancelled when the binding
     * ends, but a `catch (Throwable)` on its way swallows that and runs on to
     * the next suspension, and [ensureSession] is what stops it there.
     */
    private fun CoroutineScope.launchSession(block: suspend CoroutineScope.() -> Unit): kotlinx.coroutines.Job {
        val work = SessionWork(sessionBinding.capture())
        return launchBound(sessionBinding) { withContext(work, block) }
    }

    /** UX 9 — the removals noticed in this session that no one has seen yet:
     *  in memory only, gone with the process. */
    private val liveEndNotices = LinkedHashMap<String, MembershipEndNotice>()

    init {
        // ADR-0073 R.5 — a lineage register that arrives (this device's own
        // write, or another device's over the DeviceGroup) changes which
        // groups are one conversation: the folded list is published again.
        pushScope.launch {
            LineageMirror.version.drop(1).collect {
                try {
                    if (mutableState.value.bootstrapped) publishThreadList()
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (_: Throwable) {
                    // the next mutation publishes
                }
            }
        }
    }

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
     * The account whose message log the records in [groups] are, or `null`
     * while they are not a whole log: before [bootstrap] has seeded them from
     * disk, and from the moment [wipeInMemoryState] empties them.
     *
     * [persistMessageLogBestEffort] writes the WHOLE log from [groups], and it
     * used to pair them with the account id read a statement earlier. A persist
     * that straddled a sign-out wrote an empty log under the account that had
     * just left, erasing its history on disk; one that ran while the next
     * account was still loading wrote that account a partial log, which the
     * bootstrap then read back. Read and written only under `synchronized(groups)`.
     */
    private var messageLogAccount: String? = null

    /**
     * The log's slots for groups not in [groups]: a pairing history bundle
     * lands before the Welcomes do. [persistMessageLogBestEffort] writes them
     * back, and a join takes its slot ([registerJoinedGroup]). Guarded by
     * `synchronized(groups)`, like [messageLogAccount].
     */
    private val waitingLogSlots = WaitingLogSlots()

    /** Bound at construction, dropped at [close]; see [adoptHistoryBundle]. */
    private val historyBundleSink: (String, Map<String, PersistedGroup>) -> Unit = { account, slots ->
        adoptHistoryBundle(account, slots)
    }

    init {
        // After the property above: a history bundle landing while this
        // repository runs comes here.
        me.lakko.app.devices.HistoryBundleCodec.liveLogSink = historyBundleSink
        // ADR-0091 phase 2 route 1 — another device of this account asks to
        // be brought back into a conversation.
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.conversationReentrySink = { envelope, _ ->
            receiveConversationReentry(envelope, ReentryRouteKind.OWN_DEVICE)
        }
    }

    /**
     * ADR-0050 — the ACCOUNT identity (phrase-seeded), kept beside the DEVICE
     * identity [canonicalIdentity] for what is about the person rather than a
     * leaf: invite capabilities and their revocation. Every group, every
     * KeyPackage and the MLS snapshot use [canonicalIdentity], which since
     * ADR-0050 is this device's bound leaf identity.
     */
    private val accountIdentity = AtomicReference<Identity?>(null)

    /** ADR-0074 part 3 - the account identity the directory serves, when it
     *  is NOT the conversation identity ([accountIdentity]): set after a
     *  rotation of the account keys, `null` otherwise. Used only for the
     *  mailbox of the published key, the reach keys and the journal roots
     *  ([rootAccountIdentity]). */
    private val publishedAccountIdentity = AtomicReference<Identity?>(null)

    /**
     * ADR-0074 section 3.5, "DS" - the account identity the ROOT of an owned
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
     *  - worse, `restoreStorage` replaces every group's state at once (the
     *    ADR-0040 F1 abort path used it until the ADR-0081 amendment of
     *    2026-10-19 deleted the direct commits it served). Running it while
     *    another group sits at an un-persisted N+1 would ROLL THAT GROUP BACK,
     *    re-opening the exact nonce-reuse / device-behind window ADR-0040
     *    closes.
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
    public suspend fun bootstrap(secrets: IdentitySecrets) {
        // On the caller's thread, before the hop: another account's work ends
        // here, and the binding this bootstrap runs for is captured before
        // [clear] can land. The restore below is one long blocking stretch
        // that no cancellation interrupts, so it asks the binding instead.
        val switching = currentAccountIdB64.get()
            ?.let { it != base64UrlSafe(secrets.accountId) } ?: false
        // Handed another account while one is installed: the session that is
        // being replaced ends exactly as a sign-out ends it (its work, its
        // per-group timers and records, its reach binding), not only its
        // groups.
        if (switching) clear()
        val bound = sessionBinding.capture()
        withContext(mlsDispatcher) { bootstrapOnMlsThread(secrets, bound) }
    }

    /**
     * ADR-0074 (a device paired after a key rotation) - the pairing state
     * bundle just stored this account's earlier keys, so the key its
     * conversation leaves are bound to is no longer the one this repository
     * loaded under. Rebuild the session under it: the device identity's
     * binding is re-minted under the conversation key
     * ([me.lakko.app.auth.SessionIdentities.deviceIdentity] re-mints a
     * binding that names another key), the MLS snapshot (keyed by the leaf
     * key, which does not change) is restored, and a KeyPackage bound to the
     * conversation key replaces the one in the directory.
     *
     * `false` when nothing was loaded for that account (the next
     * [bootstrap] reads the predecessor store itself), the loaded
     * conversation key already is the stored one, or this device holds a
     * conversation: its leaves carry the binding it has, and a new mint would
     * split it between two account keys ([me.lakko.app.auth.BindingVerdict]).
     */
    public suspend fun rebindConversationIdentity(secrets: IdentitySecrets): Boolean =
        withContext(mlsDispatcher) {
            val accountIdB64 = base64UrlSafe(secrets.accountId)
            if (currentAccountIdB64.get() != accountIdB64) return@withContext false
            if (knownGroupsStore.load(accountIdB64).isNotEmpty()) {
                android.util.Log.i("ChatRepository", "ADR-0074: this device holds conversations; its binding stays")
                return@withContext false
            }
            val seed = me.lakko.app.auth.conversationAccountSeedOf(
                applicationContext,
                accountIdB64,
                secrets.accountIdentitySeed,
            )
            val wanted = try {
                Identity.fromSeed(seed).use { it.signaturePublicKey() }
            } finally {
                seed.fill(0)
            }
            val held = accountIdentity.get()?.signaturePublicKey()
            if (held != null && held.contentEquals(wanted)) return@withContext false
            providerLock.withLock {
                // What the loaded identity holds goes to disk first: the
                // rebuilt one restores from it.
                persistSnapshotBestEffort()
                wipeInMemoryState()
                currentAccountIdB64.set(null)
                accountIdentity.getAndSet(null)?.close()
                publishedAccountIdentity.getAndSet(null)?.close()
            }
            android.util.Log.i("ChatRepository", "ADR-0074: conversation identity rebuilt under the account's earlier key")
            bootstrapOnMlsThread(secrets, sessionBinding.capture())
            true
        }

    /** ADR-0074 - the proven steps of this account's OWN key history, from
     *  its stored earlier keys to the key the session holds: recorded by the
     *  rotation or the adoption on a device that lived through it, and here
     *  for a device that received the earlier keys in a pairing state bundle
     *  before it held the current seed. Idempotent; public keys only. */
    private fun ensureOwnProvenChainBestEffort(accountIdB64: String, currentSeed: ByteArray) {
        try {
            val rows = me.lakko.app.auth.OwnPredecessorStore.onDevice(applicationContext).load(accountIdB64)
            if (rows.isEmpty()) return
            val keys = try {
                rows.map { me.lakko.app.auth.base64Encode(it.signaturePubkey) }
            } finally {
                rows.forEach { it.zero() }
            }
            val current = Identity.fromSeed(currentSeed).use { me.lakko.app.auth.base64Encode(it.signaturePublicKey()) }
            val store = me.lakko.app.contacts.SharedPrefsProvenLinkStore(applicationContext)
            for ((older, newer) in me.lakko.app.devices.StateBundle.ownChainLinks(keys, current)) {
                me.lakko.app.contacts.ProvenPredecessors.recordProven(
                    store = store,
                    ownerAccountIdB64 = accountIdB64,
                    accountIdB64 = accountIdB64,
                    oldKeyB64 = older,
                    newKeyB64 = newer,
                )
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0074: own key history not recorded: ${cause.javaClass.simpleName}")
        }
    }

    /** The key the account publishes now and its own predecessor keys, oldest
     *  first ([me.lakko.app.auth.bindingVerdict]'s inputs). */
    private fun ownBindingKeys(accountIdB64: String, currentSeed: ByteArray): Pair<ByteArray, List<ByteArray>> {
        val current = me.lakko.app.auth.SessionSecretWipe.use(currentSeed) { seed ->
            Identity.fromSeed(seed).use { it.signaturePublicKey() }
        }
        val rows = try {
            me.lakko.app.auth.OwnPredecessorStore.onDevice(applicationContext).load(accountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }
        return current to rows.map { it.signaturePubkey.copyOf() }.also { rows.forEach { row -> row.zero() } }
    }

    /** The seed of this account's own identity whose key is [key] (an own
     *  predecessor, or the current one), a fresh copy; `null` when neither. */
    private fun ownSeedNamed(accountIdB64: String, currentSeed: ByteArray, key: ByteArray): ByteArray? {
        val rows = try {
            me.lakko.app.auth.OwnPredecessorStore.onDevice(applicationContext).load(accountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }
        try {
            rows.firstOrNull { it.signaturePubkey.contentEquals(key) }?.let { return it.phraseSeed.copyOf() }
        } finally {
            rows.forEach { it.zero() }
        }
        return me.lakko.app.auth.SessionSecretWipe.use(currentSeed) { seed ->
            val matches = Identity.fromSeed(seed).use { it.signaturePublicKey().contentEquals(key) }
            if (matches) seed.copyOf() else null
        }
    }

    /** The identity of this account whose key is [key]: an own predecessor,
     *  or the current one. `null` when it is neither. */
    private fun ownIdentityNamed(accountIdB64: String, currentSeed: ByteArray, key: ByteArray): Identity? {
        val rows = try {
            me.lakko.app.auth.OwnPredecessorStore.onDevice(applicationContext).load(accountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }
        try {
            rows.firstOrNull { it.signaturePubkey.contentEquals(key) }?.let { return Identity.fromSeed(it.phraseSeed) }
        } finally {
            rows.forEach { it.zero() }
        }
        val current = me.lakko.app.auth.SessionSecretWipe.use(currentSeed) { Identity.fromSeed(it) }
        if (current.signaturePublicKey().contentEquals(key)) return current
        current.close()
        return null
    }

    private suspend fun bootstrapOnMlsThread(secrets: IdentitySecrets, bound: me.lakko.app.Binding) {
        val newAccountIdB64 = base64UrlSafe(secrets.accountId)
        // Refresh the publish credentials even on the same-account early
        // return below: a re-authentication hands us a newer token.
        val publishAccount = KeyPackageAccount(
            accountIdB64 = newAccountIdB64,
            accountId = secrets.accountId.copyOf(),
            sessionToken = secrets.sessionToken,
        )
        keyPackageAccount.set(publishAccount)
        if (!bound.isCurrent) {
            keyPackageAccount.compareAndSet(publishAccount, null)
            return
        }
        val existing = currentAccountIdB64.get()
        if (existing == newAccountIdB64) {
            // ADR-0074 part 3 - the account keys were rotated in this
            // session: the published identity (and its mailbox) is the new
            // one; the conversation identity stays the predecessor.
            refreshPublishedAccountIdentity(secrets.accountIdentitySeed)
            // ADR-0053 — a re-authentication of the same account still claims
            // both mailboxes ("after every login").
            claimOwnMailboxesBestEffort()
            // ADR-0074 section 3.5, "Reach": the reach keys and the mailbox
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
        // ADR-0074 part 3, Stage A: after a rotation of the account keys the
        // conversation leaves that exist stay bound to the PREDECESSOR key
        // (no leaf is moved in place), so the account identity the chat
        // layer acts as - role tables, registrar-seed envelopes, the mailbox
        // Welcomes for those bindings arrive in - is the predecessor's. The
        // identity the directory serves is kept beside it for what derives
        // from the seed outside MLS: the mailbox of the published key, the
        // reach keys and the journal roots of owned groups (section 3.5).
        // Read through [SessionSecretWipe]: a sign-out wipes the session's
        // seed in place, and this must see it whole or be told the session
        // ended, never zeroes.
        val conversationSeed = me.lakko.app.auth.SessionSecretWipe.use(secrets.accountIdentitySeed) { seed ->
            me.lakko.app.auth.conversationAccountSeedOf(applicationContext, newAccountIdB64, seed)
        }
        val conversationIdentity = try {
            Identity.fromSeed(conversationSeed)
        } finally {
            conversationSeed.fill(0)
        }
        ensureOwnProvenChainBestEffort(newAccountIdB64, secrets.accountIdentitySeed)
        // ADR-0050: every conversation leaf is this device's identity, bound to
        // the account key. A missing device seed falls back to a second unbound
        // instance of the account identity (the pre-ADR-0050 behaviour).
        val ownKeys = ownBindingKeys(newAccountIdB64, secrets.accountIdentitySeed)
        // G-56: the account's own conversation link as the auth gate last read
        // it. A `wait` throws [me.lakko.app.auth.EarlierKeysMissing] before
        // anything is minted, stored or published (the gate parks the device
        // first; this is defence in depth).
        val recordedLink = me.lakko.app.auth.linkOfRecord(
            try {
                me.lakko.app.auth.OwnConversationKeyStore.onDevice(applicationContext).load(newAccountIdB64)
            } catch (_: Throwable) {
                null
            },
        )
        val identity = try {
            me.lakko.app.auth.SessionIdentities.deviceIdentity(
                applicationContext,
                newAccountIdB64,
                conversationIdentity,
                currentKey = ownKeys.first,
                predecessorKeys = ownKeys.second,
                holdsConversations = knownGroupsStore.load(newAccountIdB64).isNotEmpty(),
                link = recordedLink,
                seedOfKey = { key -> ownSeedNamed(newAccountIdB64, secrets.accountIdentitySeed, key) },
            )
        } catch (missing: me.lakko.app.auth.EarlierKeysMissing) {
            keyPackageAccount.compareAndSet(publishAccount, null)
            conversationIdentity.close()
            throw missing
        } ?: me.lakko.app.auth.SessionSecretWipe.use(secrets.accountIdentitySeed) { current ->
            me.lakko.app.auth.conversationAccountSeedOf(applicationContext, newAccountIdB64, current)
        }.let { seed ->
            try {
                Identity.fromSeed(seed)
            } finally {
                seed.fill(0)
            }
        }
        // ADR-0074 part 3 - a kept binding that names another of the account's
        // own keys than the oldest ([me.lakko.app.auth.BindingVerdict]): the
        // chat layer acts as the key its leaves actually carry.
        val account = identity.accountPublicKey()
            ?.takeIf { bound -> !bound.contentEquals(conversationIdentity.signaturePublicKey()) }
            ?.let { bound -> ownIdentityNamed(newAccountIdB64, secrets.accountIdentitySeed, bound) }
            ?.also { conversationIdentity.close() }
            ?: conversationIdentity
        refreshPublishedAccountIdentity(secrets.accountIdentitySeed, conversation = account)
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
        providerLock.sessionLock {
            // The account signed out (or another landed) while this waited:
            // install nothing.
            if (!bound.isCurrent) {
                keyPackageAccount.compareAndSet(publishAccount, null)
                identity.close()
                account.close()
                return
            }
            // Restore the snapshot — best-effort. The Identity is
            // valid even if the restore fails (we just lose the
            // prior MLS state, peers re-bootstrap).
            val restoreResult = runCatching {
                val blob = snapshotStore.getSnapshot(applicationContext, newAccountIdB64)
                // ADR-0050 — a snapshot taken under another leaf identity (every
                // pre-ADR-0050 snapshot) must not be restored: `restoreStorage`
                // replaces the whole provider with groups signed by a key this
                // identity does not hold. Start empty; the next persist
                // overwrites it under the right key. The snapshot read here is
                // only ever this device's own: the account backup carries no
                // MLS state (2026-10-02).
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
            AdmittingAccounts.bind(applicationContext, newAccountIdB64)
            // ADR-0091 phase 2 — this account's behind states (the waiting
            // note survives a restart).
            conversationBehind.bind(newAccountIdB64)
            // Walk 2, R1 — an own publish the DS accepted is a row this
            // device knows, in a conversation it holds (other journals are
            // passed over).
            me.lakko.app.ds.DsClient.groupRowAcceptedListener = { journalHex, createdAt ->
                val hex = journalHex.lowercase()
                if (groups.containsKey(hex) || groups.containsKey(journalHex)) {
                    conversationBehind.noteRowKnown(hex, me.lakko.app.ui.chat.parseMessageTimeMs(createdAt))
                }
            }
            me.lakko.app.ui.chat.ArticleDrafts.bind(applicationContext, newAccountIdB64)
            UnseenReactions.bind(applicationContext, newAccountIdB64)
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
                    // Stale: stop opening journals and provisioning push
                    // routing for an account that is gone (undone below).
                    if (!bound.isCurrent) break
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
            // ADR-0082 — this account's translations and transcripts.
            me.lakko.app.translation.TranslationCenter.install(applicationContext) {
                me.lakko.app.l10n.Localization.state.value.resolved
            }
            me.lakko.app.translation.TranslationCenter.bind(applicationContext, newAccountIdB64)
            if (log != null) {
                for ((groupIdHex, persisted) in log.groups) {
                    val record = groups[groupIdHex] ?: continue
                    val seeded = persisted.messages.mapNotNull { it.toChatMessage() }
                    record.seed(messages = seeded, cursor = persisted.cursor)
                    // ADR-0063 section 5 — what the feed showed of this group.
                    record.seedFeedSeen(persisted.feedSeen.toSet(), persisted.feedSeenThrough)
                    // "History is not news" — this group's pairing-history floor.
                    record.raiseSyncedThrough(persisted.syncedThroughMessageId)
                }
                publishThreadList()
            }
            // From here the records and the waiting slots hold everything the
            // log on disk does, so a persist may replace it (see
            // [messageLogAccount]).
            synchronized(groups) {
                waitingLogSlots.fillFrom(log?.groups.orEmpty(), groups.keys.toSet())
                messageLogAccount = newAccountIdB64
            }

            // [clear] ran while the region above was installing this account
            // (it takes no lock): take everything back out, still under the
            // provider lock, so the next bootstrap starts from nothing.
            if (!bound.isCurrent) {
                standDownStaleBootstrap(newAccountIdB64, publishAccount)
                return
            }
            // ADR-0087 section 9 — an end owed from before: its row is gone
            // from the list from the first frame, and nothing polls it.
            // UX 9 — notices belong to the session of one account.
            synchronized(liveEndNotices) { liveEndNotices.clear() }
            loadOwedEndsBestEffort(newAccountIdB64)
            mutableState.update { it.copy(bootstrapped = true) }
            // ADR-0073 R.7 case 1 — the messages held when the process died.
            restoreHeldSends()
            // State bundle v3 — the display copies already in a one-to-one.
            restoreHeldCopies()
        }
        if (!bound.isCurrent) return
        // ADR-0087 section 9 — the launch sweep over the stored groups.
        pushScope.launchSession { sweepMembershipEnds() }
        // ADR-0053 — sign reads of both own mailboxes and claim them.
        claimOwnMailboxesBestEffort()
        // ADR-0060 — bind the reach keys to this account and reconcile its
        // mailbox (the default setting for an account that never chose).
        // ADR-0074 section 3.5, "Reach": after a rotation the reach keys
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
     * Publish a KeyPackage this device can be added to a group with: mint it
     * from the canonical [Identity], persist the provider snapshot durably
     * under [providerLock], and only then upload it with the session token.
     * No upload when the persist fails (fail closed). Serialized, so a burst of
     * requests made before a run mints uploads once. See
     * [KeyPackageRepublisher] for the ordering and coalescing rules.
     */
    public suspend fun republishKeyPackage(): KeyPackageRepublishOutcome =
        forSession { keyPackageRepublisher.republish() }

    /**
     * Fire-and-forget [republishKeyPackage]. The request is numbered BEFORE
     * this returns, so a caller that just finished a Welcome join is
     * guaranteed a KeyPackage minted after that join.
     */
    public fun requestKeyPackageRepublish() {
        val ticket = keyPackageRepublisher.markRequested()
        keyPackageScope.launchSession {
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
     *   4. Seal the welcome as a mailbox row (ADR-0078) to the
     *      recipient's account mailbox, whose key comes from a binding
     *      verified against their account key, and post it via
     *      `DsClient.publishMailbox`.
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
         *  link grant of an `/i/` link, its binding already verified, ADR-0078).
         *  `null` picks from the grants this device holds for the recipient. */
        recipientReach: me.lakko.app.contacts.MailboxDest? = null,
    ): String =
        forSession {
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
        recipientReach: me.lakko.app.contacts.MailboxDest?,
    ): String {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        val created = prepareGroupOnMlsThread(
            identity,
            recipientAccountId,
            kind,
            expectedKeyB64 = outboxB64(recipientSignaturePubkey),
        )
        val createdHex = DsClient.pseudonymHex(created.groupId)
        // The founder's state is on disk BEFORE any Welcome goes out, and a
        // persist that fails or is refused (the account's session ended, its
        // store gate sealed) stops the creation with nothing sent. Published
        // first, a Welcome would land the invitee in a group whose founder
        // lost it at the next restart, or that a forgotten account wrote
        // back. The ADR-0073 successor path does the same ([prepareSuccessor]).
        persistFoundedGroupBeforeWelcome(created.group, createdHex)
        // ADR-0074 3.5a rule 1 — the recipient is the PERSON: a leaf bound
        // to a proven predecessor of the key the caller named is theirs, and
        // two keys of one person are one mailbox (written once).
        val recipientCurrent = mailboxAccountKeyOf(recipientSignaturePubkey)
        val writtenMailboxes = HashSet<String>()
        // ADR-0079 (2026-10-17) - this account's own mailbox FIRST, the
        // recipient's only after it was written (a failure throws out of the
        // loop): whenever the recipient holds the Welcome, every own device's
        // mailbox holds it too.
        val ordered = me.lakko.app.contacts.LinkClaims.welcomeOrder(created.recipientAccounts) {
            mailboxAccountKeyOf(it).contentEquals(recipientCurrent)
        }
        for (account in ordered) {
            val explicit =
                if (mailboxAccountKeyOf(account).contentEquals(recipientCurrent)) recipientReach else null
            try {
                // ADR-0078: the address comes from a verified binding (the
                // explicit grant's, else one held for the person); none known
                // stops the creation like a refused write.
                val mailbox = explicit?.let { welcomeMailboxForDest(account, it) } ?: welcomeMailboxFor(account)
                if (!writtenMailboxes.add(DsClient.pseudonymHex(mailbox))) continue
                publishWelcomeToMailbox(
                    mailbox,
                    created.welcomeEnvelope,
                    createdHex,
                    explicit,
                    accountIdHint = recipientAccountId.takeIf {
                        mailboxAccountKeyOf(account).contentEquals(recipientCurrent)
                    },
                )
            } catch (e: Throwable) {
                // Not held after all: the next bootstrap does not rebuild it.
                currentAccountIdB64.get()?.let { runCatching { knownGroupsStore.remove(it, createdHex) } }
                runCatching { created.group.close() }
                throw ChatError(dsErrorMessage(e, "DS publishMailbox (Welcome) failed"), e)
            }
        }
        return registerFoundedGroup(identity, created.group, created.groupId)
    }

    /**
     * A group with nobody else in it yet (`docs/ux` scenario 14): the owner
     * alone, plus this account's other devices. People are added afterwards
     * as in any group.
     */
    public suspend fun startGroupAlone(
        /** ADR-0087 sections 3 and 10 — run with the new group's id and knock
         *  public key BEFORE anything about the group leaves this device (the
         *  @name claim): a throw here creates nothing. */
        beforePublish: (suspend (groupId: ByteArray, knockPublicKey: ByteArray) -> Unit)? = null,
    ): String = forSession { startGroupAloneOnMlsThread(beforePublish) }

    private suspend fun startGroupAloneOnMlsThread(
        beforePublish: (suspend (groupId: ByteArray, knockPublicKey: ByteArray) -> Unit)? = null,
    ): String {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        val created = foundGroupOnMlsThread(identity, ownOtherDevices(identity), GroupKind.GROUP)
        val createdHex = DsClient.pseudonymHex(created.groupId)
        persistFoundedGroupBeforeWelcome(created.group, createdHex)
        if (beforePublish != null) {
            try {
                val account = rootAccountIdentity() ?: throw IllegalStateException("no account")
                val root = uniffi.lakko_ffi_uniffi.dsJournalRootSeed(account.ed25519Seed(), created.groupId)
                val knock = try {
                    val seed = uniffi.lakko_ffi_uniffi.groupKnockSeed(root, CURRENT_KNOCK_VERSION)
                    try {
                        uniffi.lakko_ffi_uniffi.groupKnockPublicKey(seed)
                    } finally {
                        seed.fill(0)
                    }
                } finally {
                    root.fill(0)
                }
                beforePublish(created.groupId, knock)
            } catch (e: Throwable) {
                currentAccountIdB64.get()?.let { runCatching { knownGroupsStore.remove(it, createdHex) } }
                runCatching { created.group.close() }
                throw e
            }
        }
        for (account in created.recipientAccounts) {
            try {
                publishWelcomeToMailbox(
                    welcomeMailboxFor(account),
                    created.welcomeEnvelope,
                    createdHex,
                    null,
                    accountIdHint = null,
                )
            } catch (e: Throwable) {
                currentAccountIdB64.get()?.let { runCatching { knownGroupsStore.remove(it, createdHex) } }
                runCatching { created.group.close() }
                throw ChatError(dsErrorMessage(e, "DS publishMailbox (Welcome) failed"), e)
            }
        }
        val registered = registerFoundedGroup(identity, created.group, created.groupId)
        // Section 10 — the creation row of a group this device founded.
        currentAccountIdB64.get()?.let { GroupSettingsStore.markCreated(applicationContext, it, registered) }
        return registered
    }

    /** ADR-0079 - one-to-ones founded for a won invite-link request and not
     *  yet held, by hex id. In memory; after a restart the group is loaded
     *  from the MLS snapshot the founding persisted. */
    private val preparedLinkGroups = java.util.concurrent.ConcurrentHashMap<String, Group>()

    /**
     * ADR-0079 (ordering of 2026-10-17) - step 1 of the one-to-one a device
     * founds for an invite-link request it won: ONE commit adding the
     * requester's devices AND every other device of this account (ADR-0050
     * section 2), the journal claimed, the founder's state persisted
     * fail-closed; NOTHING is sent. The caller records the returned founding
     * durably, publishes the link-founded note, and only then runs
     * [holdLinkOneToOne] and [deliverLinkWelcome]; a retry runs
     * those two again on the recorded founding and nothing is founded twice.
     */
    public suspend fun prepareLinkOneToOne(
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
    ): me.lakko.app.contacts.LinkClaims.Founded =
        forSession {
            val identity = canonicalIdentity.get()
                ?: throw IllegalStateException("ChatRepository not bootstrapped — call bootstrap() first")
            val created = prepareGroupOnMlsThread(
                identity,
                recipientAccountId,
                GroupKind.ONE_TO_ONE,
                expectedKeyB64 = outboxB64(recipientSignaturePubkey),
            )
            val hex = DsClient.pseudonymHex(created.groupId)
            persistFoundedGroupBeforeWelcome(created.group, hex)
            preparedLinkGroups[hex] = created.group
            val recipientCurrent = mailboxAccountKeyOf(recipientSignaturePubkey)
            val targets = created.recipientAccounts.map {
                me.lakko.app.contacts.LinkClaims.FoundedTarget(
                    accountKeyB64 = outboxB64(it),
                    isRecipient = mailboxAccountKeyOf(it).contentEquals(recipientCurrent),
                )
            }
            me.lakko.app.contacts.LinkClaims.Founded(
                groupIdHex = hex,
                welcomeB64 = outboxB64(created.welcomeEnvelope),
                targets = me.lakko.app.contacts.LinkClaims.welcomeOrder(targets) { it.isRecipient },
                notePosted = false,
                delivered = false,
            )
        }

    /** A founding that could not be recorded: nothing was sent and nobody
     *  will carry it on, so it does not stand. */
    public fun abandonPreparedOneToOne(groupIdHex: String) {
        currentAccountIdB64.get()?.let { runCatching { knownGroupsStore.remove(it, groupIdHex) } }
        preparedLinkGroups.remove(groupIdHex)?.let { runCatching { it.close() } }
    }

    /** ADR-0079 - make the founded one-to-one a held conversation (after a
     *  restart, from the MLS snapshot the founding persisted). `false` when
     *  it cannot be brought up; the next pass tries again. */
    public suspend fun holdLinkOneToOne(groupIdHex: String): Boolean =
        forSession {
            if (groups[groupIdHex] != null) {
                preparedLinkGroups.remove(groupIdHex)
                true
            } else {
                val identity = canonicalIdentity.get()
                val bytes = hexToBytes(groupIdHex)
                val group = if (identity == null || bytes == null) {
                    null
                } else {
                    preparedLinkGroups[groupIdHex] ?: providerLock.withLock {
                        try {
                            uniffi.lakko_ffi_uniffi.loadGroup(identity, bytes)
                        } catch (_: Throwable) {
                            null
                        }
                    }
                }
                if (identity == null || bytes == null || group == null) {
                    false
                } else {
                    registerFoundedGroup(identity, group, bytes)
                    preparedLinkGroups.remove(groupIdHex)
                    true
                }
            }
        }

    /**
     * ADR-0079 - write the founded one-to-one's Welcome in the recorded order
     * (this account's own mailbox first). A failure throws out of the loop:
     * the requester is never written after a failed own write. One write per
     * mailbox (two keys of one person are one mailbox). The requester's write
     * is the answer to their request: their reply grant, then a `contact`
     * grant, and nothing else.
     */
    public suspend fun deliverLinkWelcome(
        founded: me.lakko.app.contacts.LinkClaims.Founded,
        recipientAccountId: ByteArray,
    ): Unit =
        forSession {
            val welcome = java.util.Base64.getDecoder().decode(founded.welcomeB64)
            val written = HashSet<String>()
            for (target in founded.targets) {
                val account = java.util.Base64.getDecoder().decode(target.accountKeyB64)
                val mailbox = welcomeMailboxFor(account)
                if (!written.add(DsClient.pseudonymHex(mailbox))) continue
                publishWelcomeToMailbox(
                    mailbox,
                    welcome,
                    founded.groupIdHex,
                    accountIdHint = recipientAccountId.takeIf { target.isRecipient },
                    purpose = if (target.isRecipient) {
                        me.lakko.app.reach.WritePurpose.REPLY
                    } else {
                        me.lakko.app.reach.WritePurpose.WELCOME
                    },
                )
            }
        }

    /** ADR-0079 - write a link-founded note's Welcome to the requester's
     *  mailbox: the founding device may have written this account's mailbox
     *  and left before writing the requester's. The Welcome is the same bytes
     *  for every added leaf, and the requester ignores a duplicate. Returns
     *  the group's hex id when this device holds it, else `null`. */
    public suspend fun forwardLinkWelcome(
        note: me.lakko.app.contacts.LinkClaims.HeldNote,
        recipientAccountKey: ByteArray,
        recipientAccountId: ByteArray,
    ): String? =
        forSession {
            publishWelcomeToMailbox(
                welcomeMailboxFor(recipientAccountKey),
                java.util.Base64.getDecoder().decode(note.welcomeB64),
                note.groupIdHex,
                accountIdHint = recipientAccountId,
                purpose = me.lakko.app.reach.WritePurpose.REPLY,
            )
            note.groupIdHex.takeIf { groups[it] != null }
        }

    /**
     * The fail-closed persist [startGroupWith] runs between creating a group
     * and sending its Welcome: the MLS snapshot (which holds the new group)
     * durably, then the known-groups row so a restart after the Welcome went
     * out rebuilds the conversation. Throws, with the group handle closed,
     * when either cannot be written for the account that made the group.
     */
    private suspend fun persistFoundedGroupBeforeWelcome(group: Group, groupIdHex: String) {
        try {
            providerLock.sessionLock { persistSnapshotDurableOrThrow() }
            val accountIdB64 = currentAccountIdB64.get()
                ?: throw IllegalStateException("cannot record the new group - no account id")
            // Refused by the store gate when the session ended since the
            // snapshot was written: nothing is sent then either.
            me.lakko.app.account.AccountStoreGate.writeOrEnd(accountIdB64) {
                knownGroupsStore.add(accountIdB64, groupIdHex)
            }
            // A sign-out (and another sign-in) between the lock and here
            // would have written the next account's snapshot, not this group.
            ensureSession()
        } catch (e: kotlinx.coroutines.CancellationException) {
            runCatching { group.close() }
            throw e
        } catch (e: Throwable) {
            runCatching { group.close() }
            throw ChatError("Could not save the new conversation on this device. Nothing was sent.", e)
        }
    }

    /**
     * Make a group this device founded (its journal claimed, its Welcomes
     * delivered) a held conversation: the record, the journal signer, the
     * registrar appointments, the durable snapshot, the push routing secret
     * and this device's profile. Returns its hex id.
     */
    private suspend fun registerFoundedGroup(identity: Identity, group: Group, groupId: ByteArray): String {
        val groupIdHex = DsClient.pseudonymHex(groupId)
        // The Welcomes are out. The group is recorded only for the account
        // that founded it: signed out meanwhile, its handle is let go and
        // nothing enters the next account's map, lists or snapshot.
        try {
            ensureSession()
        } catch (stale: kotlinx.coroutines.CancellationException) {
            runCatching { group.close() }
            throw stale
        }
        groupLock(groupIdHex).sessionLock {
            providerLock.sessionLock {
                groups[groupIdHex] = GroupRecord(group = group, groupId = groupId)
                // Claimed at creation; only the signer is installed here.
                installJournalSigner(groupIdHex)
                // ADR-0057 obligation 3 — appoint the invitees (their seeds
                // are sealed into the group, which they read from the epoch
                // the Welcome lands them at). Launched; retried on open.
                reconcileAppointmentsBestEffort(groupIdHex, force = true)
                persistKnownGroupBestEffort(groupIdHex)
                persistSnapshotBestEffort()
                // Section 10 of the group privacy spec — creation counts as
                // activity. Only if absent: this also runs for a founded group
                // brought back after a restart.
                currentAccountIdB64.get()?.let {
                    ConversationJoinedStore.saveIfAbsent(applicationContext, it, groupIdHex, System.currentTimeMillis())
                }
                // ADR-0042 — the founder is the OWNER: mint the push-routing
                // secret + distribute it + subscribe under the derived push_gid.
                // G-37 — kept INSIDE the provider critical section because it
                // reads `group.ownRole` / `group.groupId` synchronously. The
                // actual routing work is LAUNCHED onto `pushScope`, so this
                // nests no locks.
                ensurePushRoutingBestEffort(group, identity)
            }
            publishThreadList()
        }
        // ADR-0048 §2 — founding a group is joining it: publish this device's
        // profile so the invitee learns the founder's name. Launched, never
        // awaited: the publish takes the per-group lock itself.
        profileRepublish.publishNow(groupIdHex)
        return groupIdHex
    }

    /**
     * Create a group with one other person locally and claim its journal;
     * NOTHING is sent to anyone. The Welcome of the founding commit is in the
     * result: [startGroupWith] delivers it at once, the ADR-0073 successor
     * path only after its claim was confirmed (R.6).
     *
     * Only the recipient's devices whose KeyPackage binding names
     * [expectedKeyB64], or a proven predecessor or successor of it, are added
     * ([personDevicesToAdd]): the directory is not trusted to say whose
     * devices it returned.
     */
    private suspend fun prepareGroupOnMlsThread(
        identity: Identity,
        recipientAccountId: ByteArray,
        kind: GroupKind,
        expectedKeyB64: String,
    ): CreatedGroup {
        // ADR-0050 — every device KeyPackage of the recipient: a person is
        // added as all of their devices, each its own leaf.
        val recipientDevices = personDevicesToAdd(recipientAccountId, expectedKeyB64)
        if (recipientDevices.isEmpty()) {
            throw ChatError(
                "recipient has no KeyPackage uploaded yet",
                IllegalStateException("fetchKeyPackages returned no device bound to the expected key"),
            )
        }
        return foundGroupOnMlsThread(identity, recipientDevices + ownOtherDevices(identity), kind)
    }

    /**
     * The founding [prepareGroupOnMlsThread] and [startGroupAloneOnMlsThread]
     * share: the group with [devices] added in one commit (none: the founder
     * alone, no commit and no Welcome), its journal claimed, nothing sent.
     */
    private suspend fun foundGroupOnMlsThread(
        identity: Identity,
        devices: List<me.lakko.app.discovery.DeviceKeyPackage>,
        kind: GroupKind,
    ): CreatedGroup {
        // The directory answered for the account that asked; nothing is
        // created for another one.
        ensureSession()

        // G-37 — the group does not exist yet, so NO other operation can be
        // targeting it and there is no per-group lock to take until its id
        // exists. Only the shared provider needs guarding here.
        val created = providerLock.sessionLock {
            val group = Group.create(identity, kind)
            // ADR-0050 section 2.6 — every device of the recipient AND this
            // account's other devices in ONE commit. The founder is the only
            // member, so the commit fans out to nobody and is dropped; the ONE
            // Welcome carries every added leaf's secrets and goes, byte-identical,
            // to each leaf's own mailbox. One commit per device would strand
            // every device but the last at a Welcome epoch nobody could reach.
            val addResult = try {
                devices.takeIf { it.isNotEmpty() }?.let { added ->
                    group.addMembersFromWire(identity, added.map { it.keyPackage })
                }
            } catch (e: FfiException) {
                group.close()
                throw ChatError("addMembersFromWire failed", e)
            }
            // ADR-0078: the raw Welcome; it is sealed per mailbox when written.
            val welcomeEnvelope = addResult?.welcome ?: ByteArray(0)
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
            // The claim landed for the account that made the group. Signed
            // out meanwhile: the group is not recorded as held by whoever is
            // signed in now (closed in the catch below).
            ensureSession()
            markRootHeld(DsClient.pseudonymHex(created.groupId))
        } catch (e: kotlinx.coroutines.CancellationException) {
            runCatching { created.group.close() }
            throw e
        } catch (e: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0057: claim at creation did not land; not created", e)
            runCatching { created.group.close() }
            throw ChatError(dsErrorMessage(e, "Could not reach the server. Nothing was sent."), e)
        }

        return created
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
     * ADR-0074 section 3.5a rule 1 — the account key whose mailbox is
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
     * addressed, with the grants held for that person (ADR-0074 3.5a).
     */
    private fun welcomeMailboxFor(accountKey: ByteArray, groupIdHex: String? = null): ByteArray {
        val current = mailboxAccountKeyOf(accountKey)
        // ADR-0078: the mailbox key comes from a binding verified against the
        // person's current account key; none known writes nothing. In a
        // group ([groupIdHex], the sweep's Welcome) a committer that holds
        // no grant for the person finds it in the delegation that names it,
        // or in the binding of a person found in Everyone (ADR-0060
        // section 6) — before, such a committer threw here and its
        // delegation was never used.
        val mailbox = try {
            me.lakko.app.reach.ReachManager.get(applicationContext)
                .welcomeMailboxPkFor(current, groupIdHex)
                ?.let(mailboxCrypto::address)
        } catch (_: Throwable) {
            null
        } ?: throw me.lakko.app.contacts.MailboxUnknown()
        welcomeTargetAccounts[DsClient.pseudonymHex(mailbox)] = current
        return mailbox
    }

    /** [welcomeMailboxFor] when the caller holds a verified [dest] for the
     *  person (an `/i/` link grant): its address, remembered the same way. */
    private fun welcomeMailboxForDest(accountKey: ByteArray, dest: me.lakko.app.contacts.MailboxDest): ByteArray {
        val mailbox = mailboxCrypto.address(dest.mailboxPk)
        welcomeTargetAccounts[DsClient.pseudonymHex(mailbox)] = mailboxAccountKeyOf(accountKey)
        return mailbox
    }

    /** ADR-0078 - the binding check and sealing of mailbox rows. A test
     *  injects a fake (the host JVM loads no native library). */
    internal var mailboxCrypto: me.lakko.app.contacts.MailboxCrypto = me.lakko.app.contacts.NativeMailboxCrypto

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
     *
     * [purpose] `REPLY` is the Welcome that answers a contact request: only
     * their reply grant, then a `contact` grant, never unsigned.
     */
    private suspend fun publishWelcomeToMailbox(
        mailbox: ByteArray,
        bytes: ByteArray,
        groupIdHex: String?,
        explicit: me.lakko.app.contacts.MailboxDest? = null,
        requireGrant: Boolean = false,
        /** The added person's account id when the caller holds it (rule 6). */
        accountIdHint: ByteArray? = null,
        purpose: me.lakko.app.reach.WritePurpose = me.lakko.app.reach.WritePurpose.WELCOME,
    ): PublishResult {
        // ADR-0074 section 3.5a rule 6 - a refused write re-checks the
        // person's address once: their identity bundle through the gate
        // (which records a proven predecessor), then the mailbox and the
        // grants are resolved again and the write is tried once more.
        val person = welcomeTargetAccounts[DsClient.pseudonymHex(mailbox)]
        var target = mailbox
        return me.lakko.app.reach.writeRecheckingAddress(
            write = { publishWelcomeOnce(target, bytes, groupIdHex, explicit, requireGrant, purpose) },
            isRefusal = { it is me.lakko.app.ds.DsReachRefusedError || it is WelcomeUndeliverable },
            recheck = recheck@{
                val account = person ?: return@recheck false
                val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
                if (account.contentEquals(reach.ownAccountKey() ?: ByteArray(0))) return@recheck false
                val accountHex = DsClient.pseudonymHex(account)
                val nowMs = System.currentTimeMillis()
                val due = me.lakko.app.reach.addressRecheckDue(addressRecheckedAtMs[accountHex], nowMs)
                val checked = if (due) {
                    addressRecheckedAtMs[accountHex] = nowMs
                    addressRecheck?.invoke(account, accountIdHint) == true
                } else {
                    false
                }
                val next = try {
                    welcomeMailboxFor(account, groupIdHex)
                } catch (_: me.lakko.app.contacts.MailboxUnknown) {
                    target
                }
                val moved = !next.contentEquals(target)
                target = next
                checked || moved
            },
        )
    }

    /** When the address of an account (key hex) was last re-checked after a
     *  refused write, so an owed Welcome retried at every poll does not fetch
     *  the person's bundle at every poll. In memory. */
    private val addressRecheckedAtMs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /**
     * ADR-0074 section 3.5a rule 6 - fetch the identity bundle of the person
     * known by an account key through the fail-closed gate; `true` when it
     * passed. Set by the contacts repository at construction; `null` runs
     * no check (tests).
     */
    @Volatile
    public var addressRecheck: (suspend (accountKey: ByteArray, accountIdHint: ByteArray?) -> Boolean)? = null

    /**
     * Rule 5 - the rotating device's owed step: revoke the links of the
     * previous key and re-send the unanswered contact requests. Set by the
     * contacts repository at construction; `null` leaves the step owed.
     */
    @Volatile
    public var spendPreRotationGrants: (suspend () -> Boolean)? = null

    /** One attempt of [publishWelcomeToMailbox]. [bytes] is the raw Welcome:
     *  it is sealed here (ADR-0078) to the mailbox key the authorising
     *  grant's binding names, verified against the person's current account
     *  key, and posted to that key's address. */
    private suspend fun publishWelcomeOnce(
        mailbox: ByteArray,
        bytes: ByteArray,
        groupIdHex: String?,
        explicit: me.lakko.app.contacts.MailboxDest?,
        requireGrant: Boolean,
        purpose: me.lakko.app.reach.WritePurpose,
    ): PublishResult {
        // Called once per recipient, each after the previous publish: the
        // grants below are the ones the signed-in account holds, and a
        // Welcome is written with the sender's own.
        ensureSession()
        if (explicit != null) return writeSealedWelcome(explicit, bytes)
        // ADR-0078: no account behind the address means no binding to seal
        // to: nothing is written (never an unsealed or guessed write).
        val account = welcomeTargetAccounts[DsClient.pseudonymHex(mailbox)]
            ?: throw me.lakko.app.contacts.MailboxUnknown()
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        if (account.contentEquals(reach.ownAccountKey() ?: ByteArray(0))) {
            // Another device of this account: its own `contact` key, as before.
            val own = reach.reachAuthFor(account, me.lakko.app.reach.WritePurpose.WELCOME, groupIdHex)
                ?: throw me.lakko.app.contacts.MailboxUnknown()
            return writeSealedWelcome(own, bytes)
        }
        val accountHex = DsClient.pseudonymHex(account)
        val answering = purpose == me.lakko.app.reach.WritePurpose.REPLY
        return try {
            writeWelcomeWithGrants(
                candidates = reach.reachAuthCandidates(account, purpose, groupIdHex),
                write = { dest ->
                    val target = dest ?: run {
                        android.util.Log.i("ChatRepository", "ADR-0060: Welcome written without a reach grant")
                        me.lakko.app.contacts.MailboxDest(
                            reach.mailboxPkFor(account) ?: throw me.lakko.app.contacts.MailboxUnknown(),
                            null,
                        )
                    }
                    writeSealedWelcome(target, bytes)
                },
                isRefusal = { it is me.lakko.app.ds.DsReachRefusedError },
                requireGrant = requireGrant || answering,
                accountKeyHex = accountHex,
                // ADR-0060 section 6 (amended 2026-10-29) — only when this
                // device holds no grant or delegation for the person at all
                // (a refused one keeps the Welcome owed): a person found in
                // Everyone, named by this device's record or a kept
                // no-delegation envelope, is written proofless into their
                // open mailbox (one `mailbox-write` token). Never for anyone
                // else.
                openFallback = groupIdHex?.takeUnless { answering }?.let { group ->
                    {
                        reach.openWelcomeMailboxPk(group, account)?.let {
                            android.util.Log.i("ChatRepository", "ADR-0060: Welcome written to an open mailbox")
                            me.lakko.app.contacts.MailboxDest(it, null)
                        }
                    }
                },
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
        // ADR-0060 section 6 — a person with nothing to send may have just
        // gained a grant: the next poll asks again.
        committerEnvelopeNothingToSend.forgetAll()
        val groupsOwed = undeliveredWelcomeGroups.remove(DsClient.pseudonymHex(ownerKey)) ?: return
        for (groupIdHex in groupsOwed) {
            pushScope.launchSession {
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

    /** Seal the raw Welcome [welcome] to [dest] and post it (ADR-0078). */
    private suspend fun writeSealedWelcome(dest: me.lakko.app.contacts.MailboxDest, welcome: ByteArray): PublishResult {
        val sealed = mailboxCrypto.sealFor(dest, me.lakko.app.contacts.MailboxRowKind.WELCOME, welcome)
        return dsClient.publishMailbox(sealed.address, sealed.row, reach = dest.auth)
    }

    /** The sweep's mailbox write: [publishWelcomeToMailbox] with a grant
     *  required, once per added account and bytes per session. [account] is
     *  the added ACCOUNT key (ADR-0078: its mailbox is resolved here, from a
     *  verified binding; none known keeps the Welcome owed). */
    private suspend fun publishSweepWelcome(account: ByteArray, bytes: ByteArray, groupIdHex: String): Long {
        val key = DsClient.pseudonymHex(account) + ":" + DsClient.pseudonymHex(
            java.security.MessageDigest.getInstance("SHA-256").digest(bytes),
        )
        sweepWelcomesWritten[key]?.let { return it }
        val mailbox = try {
            welcomeMailboxFor(account, groupIdHex)
        } catch (_: me.lakko.app.contacts.MailboxUnknown) {
            undeliveredWelcomeGroups.getOrPut(DsClient.pseudonymHex(mailboxAccountKeyOf(account))) {
                java.util.concurrent.ConcurrentHashMap.newKeySet()
            }.add(groupIdHex)
            throw WelcomeUndeliverable(DsClient.pseudonymHex(account))
        }
        val id = publishWelcomeToMailbox(mailbox, bytes, groupIdHex, requireGrant = true).id
        sweepWelcomesWritten[key] = id
        // The reply grant stays until it expires (7 days): a Welcome re-sent
        // after a crash, or after a refused write and an address re-check,
        // needs it. It is withdrawn only when the request is refused.
        return id
    }

    /** ADR-0060 §6 — where the delegation broadcasts run: they take the
     *  per-group lock, so they are launched, never awaited under it. */
    private val reachScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * ADR-0060 §6 — an Add this device proposed for [addedAccount]: when this
     * device holds a grant for the added person (their contact key, or a
     * group, channel or contacts-of-contacts key, a grant a search found them
     * through included), sign one delegation per committer candidate
     * ([candidates], owner first, then admins by rank) and send them in the
     * group, so whichever of them sweeps the Add can deliver the Welcome into
     * the added person's closed mailbox. For a person found in Everyone (no
     * grant, a binding this device recorded for this add) the envelope
     * carries the binding and no delegation: the committer writes the
     * Welcome into their open mailbox (amended 2026-10-29). Nothing is sent
     * when neither is held (another path must carry the Welcome).
     *
     * The row of [noteCommitterEnvelopeOwed] is written first, so a send that
     * fails stays due for [resendCommitterEnvelopesIfDue]. [deviceKey] is the
     * device the Add names: what ties the row to its own-request intent.
     */
    private fun sendCommitterDelegationsBestEffort(
        groupIdHex: String,
        addedAccount: ByteArray,
        deviceKey: ByteArray,
        candidates: List<ByteArray>,
    ) {
        if (candidates.isEmpty()) return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val addedKeyB64 = outboxB64(addedAccount)
        noteCommitterEnvelopeOwed(
            store = sentCommitterEnvelopes,
            accountIdB64 = accountIdB64,
            groupIdHex = groupIdHex,
            addedKeyB64 = addedKeyB64,
            deviceKeyB64 = outboxB64(deviceKey),
        )
        reachScope.launchSession {
            try {
                sendCommitterEnvelope(accountIdB64, groupIdHex, addedKeyB64, candidates)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0060 delegations not sent: ${cause.javaClass.simpleName}")
            }
        }
    }

    /** [sendCommitterEnvelopeOnce] against the live group and reach manager:
     *  the one send, first and re-send alike. Takes the per-group lock, so it
     *  is never awaited under it. */
    private suspend fun sendCommitterEnvelope(
        accountIdB64: String,
        groupIdHex: String,
        addedKeyB64: String,
        candidates: List<ByteArray>,
    ): CommitterEnvelopeSend {
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        return sendCommitterEnvelopeOnce(
            store = sentCommitterEnvelopes,
            accountIdB64 = accountIdB64,
            groupIdHex = groupIdHex,
            addedKeyB64 = addedKeyB64,
            candidatesB64 = candidates.map(::outboxB64),
            inFlight = committerEnvelopesInFlight,
            nothingToSend = committerEnvelopeNothingToSend,
            compose = { reach.composeDelegations(outboxUnb64(addedKeyB64), candidates, groupIdHex) },
            publish = { envelope ->
                val identity = canonicalIdentity.get()
                val record = groups[groupIdHex]
                val published = identity != null && record != null &&
                    broadcastControlToGroup(
                        identity = identity,
                        groupIdHex = groupIdHex,
                        group = record.group,
                        envelope = envelope,
                    ) != null
                ensureSession()
                published
            },
            nowMs = System::currentTimeMillis,
        )
    }

    /** ADR-0060 section 6 — what this device sent for the Adds it proposed. */
    private val sentCommitterEnvelopes: SentCommitterEnvelopeStore by lazy {
        SharedPrefsSentCommitterEnvelopeStore(applicationContext)
    }

    /** The envelopes being sent now, by (account, group, added person): a poll
     *  tail must not send a second one beside a first send still queued on
     *  the per-group lock. */
    private val committerEnvelopesInFlight: MutableSet<String> =
        java.util.concurrent.ConcurrentHashMap.newKeySet()

    private val committerEnvelopeNothingToSend = CommitterEnvelopeNothingToSend()

    /** Groups a re-send pass is running for: a second tail skips. */
    private val committerEnvelopeResendRunning: MutableSet<String> =
        java.util.concurrent.ConcurrentHashMap.newKeySet()

    /**
     * The committers who may sweep an Add, in rank order (owner, then admins),
     * as far as one envelope carries them: what [sendCommitterEnvelopeOnce]
     * covers, so a group with more candidates than an envelope holds does not
     * read as due at every poll. Under the provider lock. Empty on a failed
     * read.
     */
    private fun committerCandidatesLocked(group: Group): List<ByteArray> =
        try {
            group.groupRoles()?.let { roles ->
                (listOf(roles.owner) + roles.admins)
                    .sortedBy { group.committerRank(it)?.toLong() ?: Long.MAX_VALUE }
            }.orEmpty().take(uniffi.lakko_ffi_uniffi.maxCommitterDelegations().toInt())
        } catch (_: Throwable) {
            emptyList()
        }

    /**
     * ADR-0060 section 6 — at the tail of a poll, a fresh committer envelope
     * for each Add this device proposed that still waits, when the core's
     * rule says one is due. A delegation lives 23 hours and a proposal has no
     * deadline, so without this an Add that waits longer loses its way to the
     * person. A group with no record costs one store read.
     */
    private suspend fun resendCommitterEnvelopesIfDue(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        if (sentCommitterEnvelopes.load(accountIdB64, groupIdHex).isEmpty()) return
        if (deadOneToOne(groupIdHex)) return
        val record = groups[groupIdHex] ?: return
        if (!committerEnvelopeResendRunning.add(groupIdHex)) return
        try {
            // One provider region, so the roster, the pending set and the role
            // table describe the same epoch.
            val (snapshot, candidates) = providerLock.sessionLock {
                supersededSnapshotLocked(record) to committerCandidatesLocked(record.group)
            }
            val result = runCommitterEnvelopeResend(
                store = sentCommitterEnvelopes,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                ownIntents = loadOwnRequestIntents(pendingRequestObservations, accountIdB64, groupIdHex),
                snapshot = snapshot,
                candidatesB64 = candidates.map(::outboxB64),
                nowMs = System.currentTimeMillis(),
                due = ::committerEnvelopeIsDue,
                send = { addedKeyB64 -> sendCommitterEnvelope(accountIdB64, groupIdHex, addedKeyB64, candidates) },
                onFailure = { cause ->
                    android.util.Log.w("ChatRepository", "ADR-0060 delegations not re-sent: ${cause.javaClass.simpleName}")
                },
            )
            if (result.sent > 0) {
                android.util.Log.i("ChatRepository", "ADR-0060 re-sent ${result.sent} committer envelope(s)")
            }
        } finally {
            committerEnvelopeResendRunning.remove(groupIdHex)
        }
    }

    /** The core's rule (`committerEnvelopeDue`). A row that cannot be read
     *  asks nothing. */
    private fun committerEnvelopeIsDue(row: SentCommitterEnvelope, candidatesB64: List<String>, nowMs: Long): Boolean =
        try {
            uniffi.lakko_ffi_uniffi.committerEnvelopeDue(
                row.sentAtMs?.toULong(),
                row.signed,
                row.coveredB64.map(::outboxUnb64),
                candidatesB64.map(::outboxUnb64),
                nowMs.toULong(),
            )
        } catch (_: Throwable) {
            false
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
            // ADR-0078: the reply grant must name the joiner's own mailbox
            // (its binding verifies against the account the proposal's leaf
            // resolves to); one that does not is not kept.
            if (!me.lakko.app.contacts.grantBindsTo(attr.replyGrant, account, mailboxCrypto)) continue
            reach.keepHeld(account, attr.replyGrant, scopeHex = "")
            // Recorded per group, so it is cleared when the request is refused.
            recordKnockReply(DsClient.pseudonymHex(groupId), account, attr.replyGrant)
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
            // ADR-0092 section 7 — a setting chosen while this device was
            // away (or restored with a backup) reaches the stored flag here.
            reach.currentVisibility()?.let { setting ->
                if (reconcileOwnCardShareable(setting, accountIdB64)) publishOwnProfileToAllGroups()
            }
            // ADR-0074 3.5a rule 3 — a re-send the key change left owed (a
            // kept device right after adopting; any device after a restart).
            if (reach.profileResendOwed()) {
                reachScope.launch {
                    try {
                        resendProfileAfterKeyChange()
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (cause: Throwable) {
                        android.util.Log.w("ChatRepository", "ADR-0074 3.5a: profile re-send failed: ${cause.javaClass.simpleName}")
                    }
                }
            }
            // ADR-0074 3.5a rule 5 — the spend round the key change left owed:
            // on a kept device right after adopting, and on any device at
            // each session start until it found nothing left to do.
            if (reach.grantsSpendOwed()) {
                reachScope.launch {
                    try {
                        val spend = spendPreRotationGrants
                        if (spend == null || !spend()) {
                            android.util.Log.i("ChatRepository", "ADR-0074 3.5a: the spend round stays owed")
                        }
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (cause: Throwable) {
                        android.util.Log.w("ChatRepository", "ADR-0074 3.5a: spend round failed: ${cause.javaClass.simpleName}")
                    }
                }
            }
            publishThreadList()
            // ADR-0060 §4 — the introduction record and the relayed bundle,
            // now and on the periodic re-check.
            runIntroductionsBestEffort()
            startIntroductionTicker()
            // ADR-0060 §5 — a session in Everyone says so to the server, so a
            // publication a rotation, a restore or a failed send left behind
            // does not stay behind.
            refreshEveryoneBestEffort()
            // ADR-0060 §5 — the username certificate this account's profile
            // carries, so a viewer shows the username without a lookup. A
            // first certificate republishes the profile everywhere.
            val token = keyPackageAccount.get()?.sessionToken
            if (token != null && reach.usernameCertificate() == null) {
                val certFor = sessionBinding.capture()
                reachScope.launchSession {
                    try {
                        val cert = keyPackageDiscoveryFactory().use { client ->
                            client.setSessionToken(token)
                            client.fetchUsernameCertificate()
                        } ?: return@launchSession
                        // The certificate is the account's it was fetched
                        // for; the reach manager keeps it for whichever
                        // account is bound when it is handed over.
                        if (!certFor.isCurrent) return@launchSession
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
        // ADR-0092 section 7 — the profile's "my card may be shared" flag
        // follows the setting, under a newer stamp so receivers take it; the
        // republish below carries it.
        reconcileOwnCardShareable(setting)
        publishOwnProfileToAllGroups()
        visibilityListener?.invoke(setting)
    }

    /** ADR-0092 section 7 — the stored own profile's card-sharing flag made
     *  to follow [setting]; `true` when it changed (the caller republishes). */
    private fun reconcileOwnCardShareable(
        setting: uniffi.lakko_ffi_uniffi.FfiVisibilitySetting,
        accountIdB64: String? = currentAccountIdB64.get(),
    ): Boolean {
        val accountId = accountIdB64
            ?.let { runCatching { java.util.Base64.getDecoder().decode(it) }.getOrNull() }
            ?: return false
        return try {
            me.lakko.app.profile.OwnProfileStore.reconcileCardShareable(applicationContext, accountId, setting)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0092: card-sharing flag not updated: ${cause.javaClass.simpleName}")
            false
        }
    }

    /** The account id of the contact holding a one-to-one peer's key, when
     *  one is known. Installed by the contacts repository. */
    @Volatile
    internal var contactAccountId: ((peerKey: ByteArray) -> ByteArray?)? = null

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
        forSession { joinGroupFromWelcomeOnMlsThread(welcomeBytes = welcomeBytes) }

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
            // After every other attempt. A KeyPackage is a last-resort package
            // (ADR-0050 section 2.6): a join leaves it in the directory, one
            // row per device, and this keeps the row fresh rather than replacing
            // a dead one. A failed join can still be the symptom of a row that
            // no longer opens anything, and a fresh one is the only way the
            // NEXT adder can succeed. A Welcome refused for a group this device
            // still holds as active consumed nothing and is kept
            // ([holdWelcome]); the republish it costs is harmless. Numbered
            // here, after the join, so it is served by a KeyPackage minted
            // after it.
            if (!forAnotherDevice) requestKeyPackageRepublish()
        }
    }

    /**
     * ADR-0060 section 1 (2026-11-06) — the founder's account key a Welcome
     * names, read without joining (`peekWelcomeFounder`), so a row admitted by
     * a retired contact key can be dropped before anything is joined. Throws
     * when the Welcome cannot be read.
     */
    public suspend fun welcomeFounderAccountKey(welcomeBytes: ByteArray): ByteArray = forSession {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped — call bootstrap() first")
        providerLock.sessionLock {
            uniffi.lakko_ffi_uniffi.peekWelcomeFounder(welcomeBytes, identity).founderAccountKey
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
        var joinedFresh = false
        var joinedEpoch: ULong? = null
        var joinedGroup: Group? = null
        // ADR-0091 phase 2 section 5 — set when the Welcome brought this
        // device back into a conversation it fell behind in.
        var reentryKeyPackages: List<ByteArray> = emptyList()
        // ADR-0087 section 9 — one at a time with an end's run, which reads the
        // registry and the stored group across several regions: a join lands
        // before a run or after it, never between its delete and its drop.
        val groupIdHex = membershipEndRun.sessionLock {
            val joinedHex = providerLock.sessionLock {
                // ADR-0092 section 3.9 — the account that admitted this device
                // (the Welcome's sender, resolved to its account key) is the one
                // whose open-items hand-over this device trusts.
                val admitting = try {
                    uniffi.lakko_ffi_uniffi.peekWelcomeFounder(welcomeBytes, identity).founderAccountKey
                } catch (_: Throwable) {
                    null
                }
                val group = try {
                    Group.joinFromWelcome(welcomeBytes, identity)
                } catch (e: FfiException) {
                    // ADR-0091 phase 2 section 5 — a Welcome refused because
                    // this device holds the group as active is tried as a
                    // re-entry, only while a re-entry record exists for it.
                    // A refused re-entry leaves the original refusal standing
                    // (the Welcome is then held).
                    val stillActive = (e as? FfiException.MembershipGroupStillActive)?.message
                    val recorded = recordedReentryKeyPackages(stillActive)
                    if (!tryAsReentry(stillActive, if (recorded.isEmpty()) emptySet() else setOfNotNull(stillActive?.lowercase()))) {
                        throw ChatError("joinFromWelcome failed", e)
                    }
                    try {
                        Group.joinReentry(welcomeBytes, identity, recorded).also { reentryKeyPackages = recorded }
                    } catch (refused: FfiException) {
                        android.util.Log.i(REENTRY_LOG_TAG, "a Welcome was not a re-entry (${errorLogTag(refused)})")
                        throw ChatError("joinFromWelcome failed", e)
                    }
                }
                val groupId = group.groupId()
                val groupIdHex = DsClient.pseudonymHex(groupId)
                currentAccountIdB64.get()?.let { acct ->
                    AdmittingAccounts.record(applicationContext, acct, groupIdHex, admitting)
                }
                // ADR-0087 section 9 — a Welcome replaces an ended group. What it
                // replaces decides what the new record starts with.
                val old = groups[groupIdHex]
                val replacement = joinReplacement(
                    oldHeld = old != null,
                    endOwed = groupIdHex.lowercase() in endingGroups,
                    accountStillInRoster = old?.let { accountInRosterOrNull(it.group) },
                )
                // ADR-0091 phase 2 section 5 — a re-entry keeps the
                // conversation and its history whatever the replacement rule
                // says: the person never left.
                joinedFresh = replacement == JoinReplacement.FRESH && reentryKeyPackages.isEmpty()
                joinedEpoch = runCatching { group.epoch() }.getOrNull()
                joinedGroup = group
                if ((replacement == JoinReplacement.KEEP_HISTORY || reentryKeyPackages.isNotEmpty()) && old != null) {
                    // A retired leaf came back: same conversation, new handle.
                    old.replaceGroup(group)
                } else {
                    registerJoinedGroup(groupIdHex, GroupRecord(group = group, groupId = groupId))
                }
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
                // the routing work, so no locks nest here. After a replacement
                // whose stores are about to go, the secret goes with them: that
                // path provisions below, once they have.
                if (!joinedFresh) ensurePushRoutingBestEffort(group, identity)
                groupIdHex
            }
            // What was left of the membership that ended goes before anything
            // of the new one arrives.
            settleJoinedOverEnd(joinedHex, identity, joinedFresh, joinedGroup, joinedEpoch)
            // ADR-0091 phase 2 section 5 — back: the record and the suspicion
            // go, the gap line is written, the gap's messages are fetched.
            joinedGroup?.let { g ->
                if (reentryKeyPackages.isNotEmpty()) {
                    providerLock.sessionLock { onReentryLanded(joinedHex, g) }
                }
            }
            joinedHex
        }
        // ADR-0024 — a group re-joined under the same id keeps the read state
        // this device already persisted for it. Read outside the provider lock.
        seedReadWatermarksBestEffort(groupIdHex)
        // Written after the settling above, which wipes what is left of a
        // membership that ended: a rejoin gets a new moment.
        currentAccountIdB64.get()?.let {
            ConversationJoinedStore.save(applicationContext, it, groupIdHex, System.currentTimeMillis())
        }
        publishThreadList()
        // ADR-0048 §2 — publish this device's profile into the group it just
        // joined: nobody there knows its name yet.
        profileRepublish.publishNow(groupIdHex)
        // ADR-0048 trigger 4 — and ask the group for everyone else's: the
        // growth re-sends fire on the OTHER devices, and one that is away when
        // this join lands never re-sends. Launched, never awaited: the publish
        // takes the per-group lock itself.
        profileRequestScope.launchSession { requestProfilesBestEffort(groupIdHex, joinedJustNow = true) }
        // The sweep in flight listed its groups before this one existed and
        // holds them for up to its whole long poll, so this group's first rows
        // (the other person's name among them) would wait for the next sweep:
        // up to 30 s of a contact under a fingerprint and no first message.
        profileRequestScope.launchSession { pollJoinedGroup(groupIdHex) }
        // ADR-0060 §4 — a new one-to-one gets the introduction record and the
        // relayed bundle.
        runIntroductionsBestEffort()
        return groupIdHex
    }

    private suspend fun pollJoinedGroup(groupIdHex: String) {
        try {
            repeat(JOINED_GROUP_POLLS) { pollGroup(groupIdHex, waitSecs = JOINED_GROUP_POLL_WAIT_SECS) }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            // The next sweep lists this group.
            android.util.Log.w("ChatRepository", "first poll of a joined group failed: ${cause.javaClass.simpleName}")
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
     * ADR-0073 R.12 / UX 16.4 — told the moment a one-to-one becomes a dead
     * session (the other person's reset was accepted) or its conversation
     * moved to a successor, so a call live in that group ends locally (its
     * `End` frame is not published into a dead session). Installed with
     * [oneToOneCallInbound]; `null` (tests, before wiring) does nothing.
     */
    @Volatile
    internal var oneToOneMovedSink: ((groupIdHex: String) -> Unit)? = null

    /**
     * ADR-0083 section 4 — a one-to-one call's screen-share state, verified
     * against the row's MLS sender leaf. Installed with [oneToOneCallInbound];
     * invoked inside the poll's critical section with the row's DS message id.
     */
    @Volatile
    internal var oneToOneScreenShareInbound:
        ((groupIdHex: String, messageId: Long, callIdHex: String, share: uniffi.lakko_ffi_uniffi.VerifiedCallScreenShare) -> Unit)? =
        null

    /**
     * `docs/ux/calls.md` section 4.1.2 — a group call was ended for everyone by
     * the person who started it: a `CallEnd` with the reason `EndedForAll`
     * whose MLS sender, resolved to a person at receipt (C-68), is the starter
     * this device recorded from the `GroupCallStart`
     * ([me.lakko.app.calls.groupCallEndedForEveryoneBy]). The app-level group
     * call host leaves the call when it is in that very call.
     *
     * Never invoked for the same body from anyone else, for any other reason,
     * or for a call whose starter this device does not know. Invoked inside
     * the poll's critical section, like [oneToOneCallInbound]: the host only
     * hands off to its own scope.
     */
    @Volatile
    internal var groupCallEndedForEveryone:
        ((groupIdHex: String, callIdHex: String, starterPersonKey: MemberIdentity) -> Unit)? = null

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
        pushScope.launchSession {
            synchronized(groupCallPersistLock) {
                if (currentAccountIdB64.get() != accountIdB64) return@launchSession
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
        group: Group? = null,
    ) {
        val body = try {
            uniffi.lakko_ffi_uniffi.parseCallSignaling(payload)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "call signaling: parse failed, dropped", cause)
            return
        }
        if (body is uniffi.lakko_ffi_uniffi.FfiCallSignalingBody.ScreenShare) {
            routeScreenShareSignal(groupIdHex, messageId, body, senderKey, group)
            return
        }
        routeParsedCallSignaling(groupIdHex, messageId, body, createdAt, senderLeaf, senderKey, senderPersonKey)
    }

    /**
     * ADR-0083 section 4 — a one-to-one call's screen-share state. Verified
     * here, in the poll's locked apply region where the row's MLS sender was
     * resolved: signed by that sender's leaf key, for the one-to-one peer id.
     * Anything that does not verify is dropped; nothing of it reaches a call.
     */
    private fun routeScreenShareSignal(
        groupIdHex: String,
        messageId: Long,
        body: uniffi.lakko_ffi_uniffi.FfiCallSignalingBody.ScreenShare,
        senderKey: MemberIdentity?,
        group: Group?,
    ) {
        val sink = oneToOneScreenShareInbound ?: return
        val g = group ?: return
        val signerKey = senderKey?.let { runCatching { java.util.Base64.getDecoder().decode(it) }.getOrNull() } ?: return
        val verified = try {
            uniffi.lakko_ffi_uniffi.verifyCallScreenShare(
                g,
                body.callId,
                uniffi.lakko_ffi_uniffi.callScreenShareOneToOnePeerId(),
                signerKey,
                body.state,
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "call signaling: screen share refused, dropped")
            return
        }
        sink(groupIdHex, messageId, me.lakko.app.calls.callIdHexOf(body.callId), verified)
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
            // Read before the End closes the record: who started this call, and
            // whether this End is that person ending it for everyone. The same
            // body from anyone else closes the record and takes nobody out.
            val endedForEveryoneBy = me.lakko.app.calls.groupCallEndedForEveryoneBy(
                record = mutableGroupCalls.value[groupIdHex],
                body = body,
                senderPersonKey = senderPersonKey ?: senderKey,
            )
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
            if (endedForEveryoneBy != null && body is uniffi.lakko_ffi_uniffi.FfiCallSignalingBody.End) {
                groupCallEndedForEveryone?.invoke(
                    groupIdHex,
                    me.lakko.app.calls.callIdHexOf(body.callId),
                    endedForEveryoneBy,
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
        forSession { deriveCallKeyOnMlsThread(groupIdHex = groupIdHex, callId = callId) }

    private suspend fun deriveCallKeyOnMlsThread(
        groupIdHex: String,
        callId: ByteArray,
    ): CallKeyMaterial? {
        val identity = canonicalIdentity.get() ?: run {
            android.util.Log.w("ChatRepository", "deriveCallKey: not bootstrapped")
            return null
        }
        // G-37 — pure provider reads (MLS exporter + own leaf), no network.
        return providerLock.sessionLock {
            val record = groups[groupIdHex]
            if (record == null) {
                android.util.Log.w("ChatRepository", "deriveCallKey: no group handle")
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
    /**
     * The peer announcer of a group call in [groupIdHex] (ADR-0078 section 2):
     * this device signs who it is with its leaf key, and every other peer's
     * announcement is verified against the group's roster as it is when the
     * announcement arrives. `null` before bootstrap or for a call id that is
     * not 16 bytes. The reads are single-key map gets and read-only FFI calls
     * the core serialises itself.
     */
    public fun callPeerAnnouncer(groupIdHex: String, callId: ByteArray): me.lakko.app.calls.CallPeerAnnouncer? {
        val identity = canonicalIdentity.get() ?: return null
        if (callId.size != 16) return null
        return me.lakko.app.calls.CallPeerAnnouncer.forGroup(identity, callId) { groups[groupIdHex]?.group }
    }

    public suspend fun readSettledCallKey(
        groupIdHex: String,
        callId: ByteArray,
    ): me.lakko.app.calls.SettledGroupRead =
        forSession { readSettledCallKeyOnMlsThread(groupIdHex = groupIdHex, callId = callId) }

    private suspend fun readSettledCallKeyOnMlsThread(
        groupIdHex: String,
        callId: ByteArray,
    ): me.lakko.app.calls.SettledGroupRead {
        val identity = canonicalIdentity.get()
            ?: return me.lakko.app.calls.SettledGroupRead.notJoined()
        return providerLock.sessionLock {
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
     * `docs/ux/calls.md` section 4.1.2 — did THIS ACCOUNT start the recorded
     * group call [callIdHex] in [groupIdHex]? True on any device of the
     * starter; false when the record is gone, names another call, or carries no
     * starter.
     */
    internal fun groupCallStartedByThisAccount(groupIdHex: String, callIdHex: String): Boolean =
        me.lakko.app.calls.groupCallStartedBy(
            record = groupCalls.value[groupIdHex],
            callIdHex = callIdHex,
            ownPersonKey = ownGroupCallPersonKey() ?: ownGroupCallMemberKey(),
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
        forSession { publishGroupCallSignalOnMlsThread(groupIdHex = groupIdHex, body = body) }

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
            // No group-call frame wakes anyone, an End included: a group call
            // never rings a closed device, so there is no ring to stop.
            publishApplicationMessage(
                groupIdHex = groupIdHex,
                plaintextBytes = bytes,
                wake = me.lakko.app.calls.GROUP_CALL_CONTROL_WAKE,
            )
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
    /**
     * ADR-0079 section 5 — the account keys (base64) of the people in
     * [groupIdHex] when it is a one-to-one, else `null`: what a joined
     * conversation is matched to a person added through their invite link by.
     */
    public fun oneToOnePersonKeys(groupIdHex: String): List<String>? {
        if (groupKind(groupIdHex) != GroupKind.ONE_TO_ONE) return null
        val record = groups[groupIdHex] ?: return null
        return try {
            personKeysOf(record.group.members())
        } catch (_: Throwable) {
            null
        }
    }

    /** The groups this device holds (hex ids). */
    public fun heldGroupIds(): Set<String> = groups.keys.toSet()

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
        forSession { setMemberAdminOnMlsThread(groupIdHex = groupIdHex, memberSignaturePubkey = memberSignaturePubkey, makeAdmin = makeAdmin) }

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
        return groupLock(groupIdHex).sessionLock {
            val publishStep = providerLock.sessionLock {
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
                    // A refused persist publishes nothing ([persistProposalOrThrow]).
                    persistProposalOrThrow()
                    return@sessionLock suspend {
                        val publish = try {
                            dsClient.publishGroup(record.groupId, proposalBytes)
                        } catch (e: Throwable) {
                            throw ChatError(dsErrorMessage(e, "DS publishGroup (role-change proposal) failed"), e)
                        }
                        // Published for the account that asked. What follows files the row
                        // (echo filter, retry records, lists) and is that account's alone.
                        ensureSession()
                        noteOwnRequestBestEffort(groupIdHex, "role-change")
                        noteOwnIntentBestEffort(
                            groupIdHex,
                            RetryableRequest.SetAdmin(outboxB64(memberSignaturePubkey), makeAdmin),
                        )
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome
                    }
                }
                // ADR-0081 amendment (2026-10-19) — the direct-commit path for a
                // group with no role table is gone: refused, fail closed.
                throw noRoleTableRefusal("role change")
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
        forSession { transferGroupOwnershipOnMlsThread(groupIdHex = groupIdHex, newOwnerSignaturePubkey = newOwnerSignaturePubkey, outgoing = outgoing) }

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
        return groupLock(groupIdHex).sessionLock {
            val publishStep = providerLock.sessionLock {
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
                // request the peers hold. A refused persist publishes nothing
                // ([persistProposalOrThrow]).
                persistProposalOrThrow()
                suspend {
                    val publish = try {
                        dsClient.publishGroup(record.groupId, proposalBytes)
                    } catch (e: Throwable) {
                        throw ChatError(dsErrorMessage(e, "DS publishGroup (ownership-transfer proposal) failed"), e)
                    }
                    // Published for the account that asked. What follows files the row
                    // (echo filter, retry records, lists) and is that account's alone.
                    ensureSession()
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
        forSession { addMemberToGroupOnMlsThread(groupIdHex = groupIdHex, recipientAccountId = recipientAccountId, recipientSignaturePubkey = recipientSignaturePubkey) }

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
        // leaf: one proposal per device KeyPackage (a table-less group is
        // refused). The outcome reported is the last device's; every earlier
        // one was published or threw.
        // ADR-0073 rule 2 — the directory is not trusted to say whose devices
        // it returned: a device whose KeyPackage binding names another
        // account key than the expected person's (the key the caller named,
        // or a proven predecessor of it) is never added, whoever the caller.
        // ADR-0074 3.5 — the gate runs first: after a proven rotation the
        // person's devices are still bound to a predecessor of the key the
        // directory serves ([personDevicesToAdd]).
        val devices = personDevicesToAdd(recipientAccountId, outboxB64(recipientSignaturePubkey))
        if (devices.isEmpty()) {
            throw ChatError(
                "recipient has no KeyPackage uploaded yet",
                IllegalStateException("fetchKeyPackages returned no device bound to the expected key"),
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
        // ADR-0091 phase 2 section 2 — a device behind proposes nothing.
        if (behindStopsActing(groupIdHex)) {
            throw ChatError("this device is behind in this conversation", IllegalStateException("behind"))
        }
        val deviceKey = try {
            uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(keyPackageBytes)
        } catch (e: FfiException) {
            throw ChatError("keyPackageSignatureKey failed", e)
        }
        // G-37 — per-group lock spans the WHOLE op (create-commit, durable
        // record, both publishes), so two membership commits on the SAME group
        // still serialise exactly as before: A is on the wire before B is even
        // created. The provider lock covers only the provider-touching prefix.
        return groupLock(groupIdHex).sessionLock {
            val publishStep = providerLock.sessionLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                // Re-checked under the lock: a poll may have applied the very
                // commit that admitted this device while we waited.
                if (skipIfPresent && leafIndexOfSignatureKey(record.group, deviceKey) != null) {
                    return@sessionLock suspend { null as MembershipOutcome? }
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
                    // lose a request the peers hold. A refused persist
                    // publishes nothing ([persistProposalOrThrow]).
                    persistProposalOrThrow()
                    // ADR-0060 §6 — the committers who may sweep this Add, in
                    // rank order (owner, then admins), so this device can
                    // hand each of them a delegation for the added person's
                    // closed mailbox. Read under the provider lock.
                    val candidates = committerCandidatesLocked(record.group)
                    val addedAccount = try {
                        uniffi.lakko_ffi_uniffi.keyPackageAccountKey(keyPackageBytes)
                    } catch (_: Throwable) {
                        null
                    } ?: deviceKey
                    return@sessionLock suspend {
                        val publish = try {
                            dsClient.publishGroup(record.groupId, proposalBytes)
                        } catch (e: Throwable) {
                            throw ChatError(dsErrorMessage(e, "DS publishGroup (add-member proposal) failed"), e)
                        }
                        // Published for the account that asked. What follows files the row
                        // (echo filter, retry records, lists) and is that account's alone.
                        ensureSession()
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
                        sendCommitterDelegationsBestEffort(groupIdHex, addedAccount, deviceKey, candidates)
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome?
                    }
                }
                // ADR-0081 amendment (2026-10-19) — the direct-commit path for a
                // group with no role table is gone: refused, fail closed.
                throw noRoleTableRefusal("add")
            }
            publishStep()
        }
    }

    /**
     * Remove a member from an EXISTING group by leaf index: one Remove
     * proposal per leaf of the person (ADR-0045); a group with no role table
     * is refused (ADR-0081 amendment of 2026-10-19).
     *
     * ADR-0043 / ADR-0044 (**C-61**) — the push-routing re-key must start
     * strictly after the Remove commit lands, or the rotation envelope is
     * sealed at an epoch the evicted member can open and stamped at one that
     * only ties with the secret it still holds. Under ADR-0045 that commit is
     * the sweep's, so the rotation starts from the sweep's tail (the
     * committer) and from the poll's membership-shrink diff (every other
     * member), never here.
     */
    public suspend fun removeMemberFromGroup(
        groupIdHex: String,
        leafIndex: UInt,
    ): MembershipOutcome =
        forSession { removeMemberFromGroupOnMlsThread(groupIdHex = groupIdHex, leafIndex = leafIndex) }

    private suspend fun removeMemberFromGroupOnMlsThread(
        groupIdHex: String,
        leafIndex: UInt,
    ): MembershipOutcome {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        // ADR-0050 — removing a member removes the PERSON: every leaf of the
        // account the named leaf belongs to (leaving removes every own
        // device). Each leaf is a proposal; leaves are re-resolved by key
        // before each step. ADR-0045 + ADR-0081 amendment: nothing is rotated
        // here, there is no Remove commit yet; the device that sweeps the
        // proposal rotates (see [sweepIfCommitter]), every other member by the
        // poll's membership-shrink diff.
        val targets = groups[groupIdHex]?.let { record ->
            providerLock.sessionLock {
                val leaves = leavesOfSameAccount(record.group, leafIndex)
                val roster = rosterOf(record.group)
                leaves.mapNotNull { leaf ->
                    roster.firstOrNull { it.leafIndex == leaf }?.signaturePublicKey
                }
            }
        }.orEmpty()
        if (targets.size <= 1) return proposeRemoveLeaf(groupIdHex, identity, leafIndex)
        var last: MembershipOutcome? = null
        for (key in targets) {
            val leaf = groups[groupIdHex]?.let { record ->
                providerLock.sessionLock { leafIndexOfSignatureKey(record.group, key) }
            } ?: continue
            last = proposeRemoveLeaf(groupIdHex, identity, leaf)
        }
        return last ?: throw ChatError(
            "no leaf of that member is in the group any more",
            IllegalStateException("remove targets vanished"),
        )
    }

    /**
     * One leaf's Remove, as a proposal (ADR-0045); a group with no role table
     * is refused (ADR-0081 amendment of 2026-10-19).
     */
    private suspend fun proposeRemoveLeaf(
        groupIdHex: String,
        identity: Identity,
        leafIndex: UInt,
    ): MembershipOutcome =
        if (behindStopsActing(groupIdHex)) {
            // ADR-0091 phase 2 section 2 — a device behind proposes nothing.
            throw ChatError("this device is behind in this conversation", IllegalStateException("behind"))
        } else
        // G-37 — per-group lock spans the WHOLE op; the provider lock covers
        // only the provider-touching prefix, so the publish is outside it.
        groupLock(groupIdHex).sessionLock {
            val publishStep = providerLock.sessionLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                // ADR-0045 obligation 5 — propose in a role-tabled group.
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
                    // A refused persist publishes nothing ([persistProposalOrThrow]).
                    persistProposalOrThrow()
                    val intent = subjectKey?.let { key ->
                        val keyB64 = outboxB64(key)
                        if (key.contentEquals(identity.signaturePublicKey())) {
                            RetryableRequest.Leave(keyB64)
                        } else {
                            RetryableRequest.Remove(keyB64)
                        }
                    }
                    return@sessionLock suspend {
                        val publish = try {
                            dsClient.publishGroup(record.groupId, proposalBytes)
                        } catch (e: Throwable) {
                            throw ChatError(dsErrorMessage(e, "DS publishGroup (remove-member proposal) failed"), e)
                        }
                        // Published for the account that asked. What follows files the row
                        // (echo filter, retry records, lists) and is that account's alone.
                        ensureSession()
                        noteOwnRequestBestEffort(groupIdHex, "remove:$leafIndex")
                        intent?.let { noteOwnIntentBestEffort(groupIdHex, it) }
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome
                    }
                }
                // ADR-0081 amendment (2026-10-19) — the direct-commit path for a
                // group with no role table is gone: refused, fail closed.
                throw noRoleTableRefusal("remove")
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

    /** Section 8 — where a waiting request came from, or `null`. */
    public fun knockOriginFor(groupIdHex: String, joinerSignatureKey: ByteArray): KnockOrigin? =
        GroupSettingsStore.knockOrigin(
            applicationContext,
            currentAccountIdB64.get(),
            groupIdHex,
            me.lakko.app.ds.hexOf(joinerSignatureKey),
        )

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
        forSession { fetchJoinAttributionsOnMlsThread(groupIdHex = groupIdHex) }

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
        forSession { approveExternalJoinOnMlsThread(groupIdHex = groupIdHex, proposalRef = proposalRef) }

    private suspend fun approveExternalJoinOnMlsThread(
        groupIdHex: String,
        proposalRef: ByteArray,
    ): Long {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        return groupLock(groupIdHex).sessionLock {
            val (groupId, proposalBytes) = providerLock.sessionLock {
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
                // Fail-closed, like every other propose path: the proposal is in
                // this device's own pending store, and a refused persist
                // publishes nothing ([persistProposalOrThrow]).
                persistProposalOrThrow()
                // Section 5 — the approved row is behind the knock position.
                saveKnockCursorBestEffort(groupIdHex, record.knockCursor)
                Pair(record.groupId, bytes)
            }
            val publish = try {
                dsClient.publishGroup(groupId, proposalBytes)
            } catch (e: Throwable) {
                throw ChatError(dsErrorMessage(e, "DS publishGroup (approve proposal) failed"), e)
            }
            // Published for the account that asked. What follows files the row
            // (echo filter, retry records, lists) and is that account's alone.
            ensureSession()
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
        forSession { rejectExternalJoinOnMlsThread(groupIdHex = groupIdHex, proposalRef = proposalRef) }

    private suspend fun rejectExternalJoinOnMlsThread(
        groupIdHex: String,
        proposalRef: ByteArray,
    ): Unit {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        // G-37 — local-only (no DS call): provider work start to finish.
        var refusedAccount: ByteArray? = null
        groupLock(groupIdHex).sessionLock {
            providerLock.sessionLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                // The account behind the request, read before it goes, for
                // its reply grant.
                refusedAccount = try {
                    record.group.pendingExternalJoinAccountKey(proposalRef)
                } catch (_: Throwable) {
                    null
                } ?: proposalRef
                try {
                    record.group.rejectExternalJoin(identity, proposalRef)
                } catch (e: FfiException) {
                    throw ChatError("rejectExternalJoin failed", e)
                }
                // Section 5 — durably refused, then the knock position: the
                // refused row is behind it, so a restart does not keep it again.
                val durable = try {
                    persistSnapshotDurableOrThrow()
                    true
                } catch (cause: Throwable) {
                    persistSnapshotBestEffort()
                    false
                }
                if (durable) saveKnockCursorBestEffort(groupIdHex, record.knockCursor)
            }
        }
        refusedAccount?.let { releaseKnockReply(groupIdHex, it) }
    }

    /** ADR-0087 section 5 — more requests wait on the server than the pending
     *  set holds. */
    public fun knockQueueFull(groupIdHex: String): Boolean = groups[groupIdHex]?.knockQueueFull ?: false

    /**
     * ADR-0087 section 5 — `Reject all`: every retained request is rejected
     * here, and the server deletes the knock rows up to the newest this device
     * read (a request that arrived after stays).
     */
    public suspend fun rejectAllJoinRequests(groupIdHex: String) {
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        for (key in pendingExternalJoins(groupIdHex)) rejectExternalJoin(groupIdHex, key)
        val through = maxOf(record.lastSeenKnockId, record.knockCursor)
        if (through > 0) dsClient.deleteKnocks(record.groupId, through)
        record.knockCursor = through
        saveKnockCursorBestEffort(groupIdHex, through)
        record.knockQueueFull = false
        record.sawUnkeptExternalJoin = false
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
        forSession { createGroupInviteLinkOnMlsThread(groupIdHex = groupIdHex) }

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
        // ADR-0087 section 5 — the link names the group's knock key; an admin
        // the owner has not handed the seed to yet cannot mint one.
        val (knockPublicKey, knockVersion) = providerLock.sessionLock { groupKnockKeyFor(record, groupIdHex) }
            ?: throw ChatError("no knock key for this group yet", IllegalStateException("no knock seed"))
        val mint = try {
            uniffi.lakko_ffi_uniffi.composeGroupInviteCapabilityWithPreview(
                seed,
                groupId,
                0u,
                groupName,
                inviterName,
                members,
                knockPublicKey,
                knockVersion,
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
                    knockPublicKey,
                    knockVersion,
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
        // NOW; no session is a loud failure, and so is any failed upload, a
        // network one included (no link is minted; the inline form was
        // removed on 2026-10-19). `max_uses` is a soft throttle, not an
        // access control: the admin approval gate stays the boundary. The
        // link carries the fragment; `Copy link` copies the whole string.
        // ADR-0078 section 10: the ledger row carries a key minted for this
        // link alone, never the issuer's identity key; its seed is kept in
        // this device's record of the link and is the only thing that
        // revokes it.
        val (revocationSeed, revocationPubkey) = me.lakko.app.contacts.OwnLinks.freshRevocationKey()
        val upload = me.lakko.app.discovery.SealedInviteUpload(
            sealed = mint.sealed,
            revocationPubkey = revocationPubkey,
            expiryMs = 0L,
            maxUses = 0xFFFFFFFFL,
        )
        val blobId = uploadGroupInvite(
            sessionToken = keyPackageAccount.get()?.sessionToken,
            clientFactory = keyPackageDiscoveryFactory,
            upload = { client -> client.uploadGroupInvite(upload) },
            log = { message -> android.util.Log.w("ChatRepository", message) },
        )
        val accountIdB64 = currentAccountIdB64.get()
        if (accountIdB64 != null) {
            me.lakko.app.contacts.OwnLinkStores.current.put(
                accountIdB64,
                me.lakko.app.contacts.OwnLinks.record(
                    kind = me.lakko.app.contacts.OwnLink.KIND_GROUP,
                    blobId = blobId,
                    seed = revocationSeed,
                    maxUses = 0xFFFFFFFFL,
                ),
            )
        }
        // ADR-0087 phase B — filed in the account's link register too, so
        // every device of the account lists and revokes it.
        me.lakko.app.channels.GroupLinkRegister.record(
            entry = me.lakko.app.channels.LinkRegisterEntry(
                kind = uniffi.lakko_ffi_uniffi.LinkRecordKind.GROUP,
                targetId = groupId,
                label = groupName.orEmpty(),
            ),
            ledgerId = blobId,
            linkSecret = mint.linkSecret,
            revocationSeed = revocationSeed,
            capability = mint.sealed,
            maxUses = 0xFFFFFFFFL,
        )
        revocationSeed.fill(0)
        return me.lakko.app.discovery.composeSealedInviteUrl(
            origin = GROUP_INVITE_DEFAULT_ORIGIN,
            prefix = GROUP_INVITE_URL_PREFIX,
            blobId = blobId,
            linkSecret = mint.linkSecret,
        )
    }

    /**
     * ADR-0028 revoke — the issuer revokes a previously-created group invite
     * link. Returns `true` when its ledger row was revoked, `false` for a
     * malformed link, or one this device keeps no revocation key for
     * (ADR-0078 section 10: it lives until it expires).
     */
    public suspend fun revokeGroupInviteLink(link: String): Boolean =
        forSession { revokeGroupInviteLinkOnMlsThread(link = link) }

    private suspend fun revokeGroupInviteLinkOnMlsThread(link: String): Boolean {
        val accountIdB64 = currentAccountIdB64.get()
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
            is ParsedGroupInvite.Pointer -> {
                val store = me.lakko.app.contacts.OwnLinkStores.current
                val ownSlug = me.lakko.app.auth.base64UrlEncodeNoPad(parsed.blobId)
                val seed = store.get(accountIdB64, me.lakko.app.contacts.OwnLink.KIND_GROUP, ownSlug)?.seed()
                    ?: return false
                // Bearer-gated route: a client carrying the session token
                // read now (a missing session throws a ChatError as is). The
                // signature by the link's own revocation key is the
                // authorisation.
                try {
                    withSessionDiscovery(
                        sessionToken = keyPackageAccount.get()?.sessionToken,
                        what = "group invite revoke",
                        clientFactory = keyPackageDiscoveryFactory,
                    ) { client ->
                        try {
                            client.revokeGroupInvite(parsed.blobId, seed)
                        } catch (e: Throwable) {
                            throw ChatError("group invite revoke failed", e)
                        }
                    }
                } finally {
                    seed.fill(0)
                }
                store.remove(accountIdB64, me.lakko.app.contacts.OwnLink.KIND_GROUP, ownSlug)
                true
            }
        }
    }

    /**
     * ADR-0028 (G-3) — resolve a `/g/<slug>` slug (a 6-byte ledger id) to the
     * SEALED link bytes, fetched from the discovery ledger. Returns the sealed
     * bytes + the `blobId` so the caller can open them under the fragment
     * ([inspectGroupInvite]) and drive the consume gate before requesting to
     * join. Any other slug (an inline sealed blob, the form removed on
     * 2026-10-19, included) is malformed.
     */
    public suspend fun resolveGroupInviteSlug(slug: String): Pair<ByteArray, ByteArray> =
        forSession { resolveGroupInviteSlugOnMlsThread(slug = slug) }

    private suspend fun resolveGroupInviteSlugOnMlsThread(slug: String): Pair<ByteArray, ByteArray> {
        val parsed = try {
            parseGroupInviteSlug(slug)
        } catch (e: GroupInviteUrlException) {
            throw ChatError("group invite link is malformed", e)
        }
        return when (parsed) {
            is ParsedGroupInvite.Pointer -> {
                val fetched = try {
                    discovery.fetchGroupInvite(parsed.blobId)
                } catch (e: Throwable) {
                    me.lakko.app.rethrowIfCancelled(e)
                    throw ChatError("group invite fetch failed", InviteLinkUnreachable(e))
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
    ): GroupInviteCard = forSession {
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
                        channelId = it.channelId,
                        subscriber = it.subscriber,
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
                    groupIdHex = DsClient.pseudonymHex(cap.groupId()).lowercase(),
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
        blobId: ByteArray,
    ): Pair<ByteArray, ByteArray> =
        forSession { requestToJoinFromLinkOnMlsThread(capabilityWire = capabilityWire, nowMs = nowMs, blobId = blobId) }

    private suspend fun requestToJoinFromLinkOnMlsThread(
        capabilityWire: ByteArray,
        nowMs: ULong,
        blobId: ByteArray,
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
        val knockKey: uniffi.lakko_ffi_uniffi.FfiGroupKnockKey?
        try {
            valid = cap.isValidNow(nowMs)
            groupId = cap.groupId()
            knockKey = cap.knock()
        } finally {
            cap.close()
        }
        // ADR-0087 section 5 — the request is sealed to the knock key the link
        // names; a link without one cannot carry a request.
        if (knockKey == null) {
            throw ChatError("group invite link names no knock key", IllegalStateException("no knock key"))
        }
        if (!valid) {
            throw ChatError(
                "group invite link has expired",
                IllegalStateException("expired"),
            )
        }
        // ADR-0028 OQ1 -- register one consume against the ledger's soft
        // `max_uses` ceiling before posting the request (anti-abuse
        // defence-in-depth; the admin approval gate remains the real
        // boundary). A repeat from this joiner is idempotent.
        val accountIdB64 = currentAccountIdB64.get()
        if (accountIdB64 != null) {
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
        // The reply grant is minted by the reach manager of the account that
        // is asking to join, not of one that signed in since.
        ensureSession()
        val joinReplyGrant = try {
            me.lakko.app.reach.ReachManager.get(applicationContext).mintReply(toKey = groupId)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            throw ChatError(dsErrorMessage(e, "Could not prepare the request. Try again."), e)
        }
        // ADR-0087 section 5 — the request is a fresh KeyPackage sealed to the
        // group's knock key with the reply grant, a profile and the link's
        // capability; it carries no epoch, so it waits through every commit.
        // One knock, one `ds-knock` token (ADR-0054).
        try {
            val row = providerLock.sessionLock {
                val keyPackage = identity.createKeyPackage()
                // The KeyPackage's private key is the only thing that can open
                // the Welcome the sweep sends later: durable BEFORE the knock,
                // fail-closed.
                persistSnapshotDurableOrThrow()
                uniffi.lakko_ffi_uniffi.sealKnockRow(
                    knockKey.publicKey,
                    knockKey.version,
                    keyPackage,
                    joinReplyGrant,
                    ownKnockProfile(),
                    capabilityWire,
                )
            }
            dsClient.knock(groupId, listOf(row))
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: FfiException) {
            throw ChatError("knock row could not be sealed", e)
        } catch (e: Throwable) {
            throw ChatError(dsErrorMessage(e, "DS knock (request to join) failed"), e)
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
        forSession { pollForJoinWelcomeOnMlsThread(groupId = groupId) }

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
     * Fires a push wake when the caller asked for one ([wake]) AND the frame
     * is a call offer or end ([me.lakko.app.calls.shouldWakeForCallSignaling]),
     * so a backgrounded or closed callee rings (`docs/ux/calls.md` §2.1 / §7).
     * A 1x1 session asks only for its dial offer and for the End of a call
     * that never connected: a renegotiation offer and the End of a connected
     * call carry the same tags, and a wake on either rang the peer's iPhone.
     * The group-call control frames that reach this method through
     * [publishGroupCallSignal] ask for no wake, an End included
     * ([me.lakko.app.calls.GROUP_CALL_CONTROL_WAKE]).
     */
    public suspend fun publishApplicationMessage(
        groupIdHex: String,
        plaintextBytes: ByteArray,
        wake: me.lakko.app.calls.CallSignalingWake,
    ): Unit =
        forSession {
            publishApplicationMessageOnMlsThread(
                groupIdHex = groupIdHex,
                plaintextBytes = plaintextBytes,
                wake = wake,
            )
        }

    private suspend fun publishApplicationMessageOnMlsThread(
        groupIdHex: String,
        plaintextBytes: ByteArray,
        wake: me.lakko.app.calls.CallSignalingWake,
    ): Unit {
        // ADR-0073 R.7 case 1 — nothing goes into a dead one-to-one but the claim.
        if (deadOneToOne(groupIdHex)) throw deadSessionError()
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        groupLock(groupIdHex).sessionLock {
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
            val prepared = providerLock.sessionLock {
                chatClient.prepareSend(
                    identity = identity,
                    group = record.group,
                    plaintext = plaintextBytes,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
            }
            // docs/ux/calls.md §2.1 — a call must arrive when the app is closed.
            // ADR-0054 — the wake rides this signed publish as a header and the
            // DS fans it out after appending the row; only an offer or an end
            // the caller asked a wake for gets one, every other frame published
            // here stays silent.
            val sealedWake = sealedWakeFor(
                groupIdHex,
                if (me.lakko.app.calls.shouldWakeForCallSignaling(plaintextBytes, wake)) DsWakeKind.CALL else null,
            )
            // ADR-0045 — the publish is INSIDE the per-group lock (see
            // [runGroupSend]): a message sealed at one epoch and published on the
            // far side of a commit is permanently undecryptable for every
            // recipient, because `lakko-mls` retains no past-epoch secrets.
            chatClient.publishPreparedSend(prepared, wake = sealedWake)
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
        /** The message's row is in the conversation (it reads "Sending...",
         *  then "Not sent" with a Retry when the publish fails). */
        onRowShown: () -> Unit = {},
        /** ADR-0087 section 14 — a channel invitation card beside the text
         *  (the text is the link, so a reader without cards still has it). */
        channelInvite: ByteArray? = null,
        /** ADR-0092 — a poll, checklist, location, contact card or article. */
        content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    ): Unit =
        forSession {
            sendMessageOnMlsThread(
                groupIdHex = groupIdHex,
                plaintext = plaintext,
                attachments = attachments,
                reply = reply,
                forward = forward,
                onRowShown = onRowShown,
                channelInvite = channelInvite,
                content = content,
            )
        }

    private suspend fun sendMessageOnMlsThread(
        groupIdHex: String,
        plaintext: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
        reply: ReplyRef? = null,
        forward: ForwardRef? = null,
        onRowShown: () -> Unit = {},
        channelInvite: ByteArray? = null,
        content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    ): Unit {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        // ADR-0073 R.7 case 1 — a message is never sent into a dead
        // one-to-one: it would reach only removed devices. It is HELD: the
        // row shows the ordinary sending state and goes out when the
        // conversation's successor exists on this device.
        if (deadOneToOne(groupIdHex)) {
            holdForSuccessor(groupIdHex, plaintext, attachments, reply, forward, content)
            return
        }
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        // ADR-0091 phase 2 section 2 — once confirmed behind, nothing is
        // sent (the composer is replaced by the waiting note).
        if (conversationBehind.isConfirmed(groupIdHex)) {
            throw ChatError(
                "this device is behind in this conversation",
                IllegalStateException("behind"),
                userText = me.lakko.app.l10n.UiText("chat.reentry.waiting"),
            )
        }

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
            channelInvite = channelInvite,
            content = content,
        )
        // ADR-0092 section 3.4 — an own anonymous poll's ring version 0 is
        // the roster it is sealed under, which is the current one.
        if (content.isAnonymousPoll()) {
            val anon = (content as uniffi.lakko_ffi_uniffi.MessageContent.Poll).poll.body.anonymous!!
            record.setRing(localId, ringBytesIfMatches(rosterAccountKeysOf(record.group), anon.ring0Hash))
        }
        publishThreadList()
        onRowShown()
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
                channelInvite = channelInvite,
                content = content,
            )
        } catch (e: Throwable) {
            record.markFailed(localId)
            publishThreadList()
            // ADR-0091 phase 2 section 1 — a refused publish is evidence.
            if (proofRefusedWithClockOk(e)) conversationBehind.notePublishRefused(groupIdHex)
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
        forSession { retrySendOnMlsThread(groupIdHex, localId) }

    private suspend fun retrySendOnMlsThread(groupIdHex: String, localId: Long) {
        // ADR-0073 R.7 case 1 — the ordinary Retry of a message that gave up
        // while held: held again, never sent into the dead one-to-one.
        if (deadOneToOne(groupIdHex)) {
            retryHeld(groupIdHex, localId)
            return
        }
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        val row = record.snapshot().firstOrNull { it.localId == localId } ?: return
        if (row.ack != Ack.FAILED) return
        // ADR-0092 section 3.4 — an anonymous poll sealed under a roster that
        // has since changed cannot go as it is: the row stays failed and the
        // retry is refused, never sent without its content.
        if (!retryableAsIs(row.content) { hash -> ringBytesIfMatches(rosterAccountKeysOf(record.group), hash) != null }) {
            throw anonymousPollResendRefused()
        }
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
                channelInvite = row.channelInvite,
                content = row.content,
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
        channelInvite: ByteArray? = null,
        content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    ) {
        flushOwedProfileBeforeSend(groupIdHex)
        groupLock(groupIdHex).sessionLock {
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
                if (content != null) {
                    // ADR-0092 — a content message; a reply's quote (or, for a
                    // forward, the attribution) rides beside it.
                    uniffi.lakko_ffi_uniffi.composeContentEnvelope(
                        content,
                        attachments,
                        uniffi.lakko_ffi_uniffi.ContentEnvelopeParts(
                            replyTo = reply?.let { if (it.target < 0) REPLY_NO_JUMP_TARGET else it.target },
                            replyAuthor = reply?.author,
                            replyExcerpt = reply?.excerpt,
                            // ADR-0092 / UX 11a — a forwarded location, card
                            // or article carries the ordinary forward line.
                            forwardedFrom = forward?.author,
                            forwardOriginGroupB64 = forward?.originGroupIdHex?.let(::forwardOriginGroupIdB64From),
                            forwardOriginMessageId = forward?.originMessageId,
                        ),
                    )
                } else if (channelInvite != null) {
                    // ADR-0087 section 14 — the card rides beside the text.
                    uniffi.lakko_ffi_uniffi.composeChannelInviteEnvelope(plaintext, channelInvite)
                } else if (forward != null) {
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
                        // ADR-0073 rule 7 — a quote of a row of an older
                        // segment keeps its snapshot and carries no jump
                        // target (0): the id belongs to another journal.
                        replyTo = if (reply.target < 0) REPLY_NO_JUMP_TARGET else reply.target,
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
            val (prepared, ownKey) = providerLock.sessionLock {
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
            val wake = sealedWakeFor(groupIdHex, DsWakeKind.MESSAGE)
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

    // ---------------------------------------------------------- ADR-0092 ops

    /**
     * ADR-0092 — publish an op on a poll or checklist (vote, close, tick, add,
     * edit) and fold it into the target row at once: the DS never echoes an
     * own application message back.
     */
    public suspend fun sendContentOp(
        groupIdHex: String,
        target: Long,
        kind: ContentOpKind,
        envelope: ByteArray,
        choices: ByteArray? = null,
        item: uniffi.lakko_ffi_uniffi.ChecklistItemRef? = null,
        done: Boolean? = null,
        text: String = "",
        content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
    ): Unit = forSession {
        if (target <= 0) return@forSession
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError("no live Group handle for $groupIdHex", IllegalStateException("unknown group"))
        if (deadOneToOne(groupIdHex)) throw deadSessionError()
        // Walk 3, W3-R5 — refused while behind, before anything is folded.
        requirePersonActionAllowed(groupIdHex)
        groupLock(groupIdHex).sessionLock {
            val (prepared, ownKey) = providerLock.sessionLock {
                val prepared = chatClient.prepareSend(
                    identity = identity,
                    group = record.group,
                    plaintext = envelope,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
                prepared to ownIdentity(record.group)
            }
            val result = withPublishEvidence(groupIdHex) { chatClient.publishPreparedSend(prepared, wake = null) }
            val sender = ownKey ?: return@sessionLock
            record.addContentOp(
                target,
                ContentOpRecord(
                    kind = kind,
                    opId = result.messageId,
                    createdAtMs = isoToMs(result.createdAt),
                    sender = sender,
                    privileged = record.ownIsPrivileged(),
                    choices = choices,
                    item = item,
                    done = done,
                    text = text,
                    content = content,
                    attachments = attachments,
                ),
            )
            persistMessageLogBestEffort()
            publishThreadList()
        }
    }

    /** The current row of [messageId] in [groupIdHex], or `null`. */
    public fun contentRow(groupIdHex: String, messageId: Long): ChatMessage? =
        groups[groupIdHex]?.messageById(messageId)

    /** The raw group id of [groupIdHex], for the content folds. */
    public fun groupIdOf(groupIdHex: String): ByteArray? = groups[groupIdHex]?.groupId

    /** This device's account identity as a member key, for the content folds. */
    public fun ownMemberKey(groupIdHex: String): MemberIdentity? = groups[groupIdHex]?.let { r ->
        try {
            ownIdentity(r.group)
        } catch (_: Throwable) {
            null
        }
    }

    /** The account seed the own conversation leaves are bound to (signs polls, checklists). */
    internal fun conversationSeed(): ByteArray? = canonicalIdentity.get()?.ed25519Seed()

    /** The distinct account keys of [groupIdHex]'s roster, for a new anonymous poll's ring. */
    public fun rosterAccountKeys(groupIdHex: String): List<ByteArray> =
        groups[groupIdHex]?.let { rosterAccountKeysOf(it.group) } ?: emptyList()

    /**
     * ADR-0092 section 3.6 / UX 7.5 — cast an anonymous vote. The vote shows
     * as cast at once ("Sending your vote..."); the proof is computed off the
     * UI thread and the row leaves after a uniform random wait of 0..=10 s
     * ([BallotSchedule]). A failure leaves the vote marked failed, offered
     * again with Try again and at the next open.
     */
    public fun castAnonymousVote(
        groupIdHex: String,
        pollMessageId: Long,
        choices: ByteArray,
        /** The set version this account recorded at its first vote (any device). */
        recordedSetVersion: UInt? = null,
        onSent: (UInt) -> Unit = {},
    ) {
        val record = groups[groupIdHex] ?: return
        val previous = record.messageById(pollMessageId)?.contentExtras?.ownBallot
        val knownVersion = recordedSetVersion ?: previous?.setVersion
        record.setOwnBallot(pollMessageId, OwnBallot(choices, OwnBallotStatus.Sending, knownVersion))
        publishThreadList()
        ballotScope.launch {
            val outcome = runCatching {
                kotlinx.coroutines.delay(BallotSchedule.delayMs())
                forSession { postBallot(groupIdHex, pollMessageId, choices, knownVersion) }
            }
            outcome.getOrNull()?.let(onSent)
            val rec = groups[groupIdHex] ?: return@launch
            val setVersion = outcome.getOrNull()
            rec.setOwnBallot(
                pollMessageId,
                OwnBallot(
                    choices,
                    if (outcome.isSuccess) OwnBallotStatus.Sent else OwnBallotStatus.Failed,
                    setVersion ?: knownVersion,
                ),
            )
            persistMessageLogBestEffort()
            publishThreadList()
        }
    }

    /** Re-send every own anonymous vote left failed (UX 7.5, at the next open). */
    public fun retryFailedBallots(groupIdHex: String) {
        val record = groups[groupIdHex] ?: return
        for (m in record.snapshot()) {
            val own = m.contentExtras.ownBallot ?: continue
            if (own.status == OwnBallotStatus.Failed) {
                castAnonymousVote(groupIdHex, m.messageId, own.choices, own.setVersion)
            }
        }
    }

    private suspend fun postBallot(
        groupIdHex: String,
        pollMessageId: Long,
        choices: ByteArray,
        setVersion: UInt?,
    ): UInt {
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        val row = record.messageById(pollMessageId) ?: throw IllegalStateException("unknown poll")
        val body = (row.content as? uniffi.lakko_ffi_uniffi.MessageContent.Poll)?.poll?.body
            ?: throw IllegalStateException("not a poll")
        val anon = body.anonymous ?: throw IllegalStateException("not anonymous")
        val ringBytes = row.contentExtras.ring ?: throw IllegalStateException("no ring")
        val seed = conversationSeed() ?: throw IllegalStateException("no identity")
        val cast = try {
            kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
                uniffi.lakko_ffi_uniffi.PollBallotKeys(record.groupId, body.id, anon.secret).use { keys ->
                    uniffi.lakko_ffi_uniffi.PollRing.decode(ringBytes).use { ring ->
                        uniffi.lakko_ffi_uniffi.PollVoterKey.fromEd25519Seed(seed).use { voter ->
                            uniffi.lakko_ffi_uniffi.castPollBallot(keys, ring, voter, choices, setVersion)
                        }
                    }
                }
            }
        } finally {
            seed.fill(0)
        }
        val result = groupLock(groupIdHex).sessionLock {
            chatClient.publishBallotRow(record.groupId, cast.row)
        }
        record.attachBallot(result.messageId, result.createdAt, cast.row)
        return cast.setVersion
    }

    private val ballotScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

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
        forSession {
            personAction(groupIdHex) {
                editMessageOnMlsThread(groupIdHex = groupIdHex, targetMessageId = targetMessageId, newText = newText)
            }
        }

    private suspend fun editMessageOnMlsThread(
        groupIdHex: String,
        targetMessageId: Long,
        newText: String,
    ): Unit {
        // ADR-0073 R.5 — a row of an older segment is read-only history: its
        // on-screen id is negative and names no message of this group.
        if (targetMessageId <= 0) return
        // ADR-0073 R.7 case 1 — an op on a message of a dead one-to-one is
        // applied on this device alone and never sent ([opRoute]): its target
        // exists only in the old session.
        if (deadOneToOne(groupIdHex)) {
            applyOwnOpLocally(groupIdHex, MessageOp.Edit, targetMessageId, newText, null)
            return
        }
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        groupLock(groupIdHex).sessionLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeEditEnvelope(
                target = targetMessageId,
                newText = newText,
            )
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            // G-37 — provider work under the provider lock, publish outside it.
            val (prepared, ownKey) = providerLock.sessionLock {
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
            val sent = chatClient.publishPreparedSend(prepared)
            record.applyOwnOp(
                op = MessageOp.Edit,
                target = targetMessageId,
                text = newText,
                emoji = null,
                ownKey = ownKey,
                opId = sent.messageId,
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
        forSession {
            personAction(groupIdHex) { deleteMessageOnMlsThread(groupIdHex = groupIdHex, targetMessageId = targetMessageId) }
        }

    private suspend fun deleteMessageOnMlsThread(
        groupIdHex: String,
        targetMessageId: Long,
    ): Unit {
        // ADR-0073 R.5 — a row of an older segment is read-only history: its
        // on-screen id is negative and names no message of this group.
        if (targetMessageId <= 0) return
        // ADR-0073 R.7 case 1 — an op on a message of a dead one-to-one is
        // applied on this device alone and never sent ([opRoute]): its target
        // exists only in the old session.
        if (deadOneToOne(groupIdHex)) {
            applyOwnOpLocally(groupIdHex, MessageOp.Delete, targetMessageId, "", null)
            return
        }
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        groupLock(groupIdHex).sessionLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeDeleteEnvelope(
                target = targetMessageId,
            )
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            // G-37 — provider work under the provider lock, publish outside it.
            val (prepared, ownKey) = providerLock.sessionLock {
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
            // ADR-0065 section 6 — the message's files and thumbnails, taken
            // BEFORE the row is tombstoned (the tombstone drops them).
            val ownBlobs = record.snapshot().firstOrNull { it.messageId == targetMessageId }
                ?.let { messageBlobIds(it) }.orEmpty()
            chatClient.publishPreparedSend(prepared)
            record.applyOwnOp(
                op = MessageOp.Delete,
                target = targetMessageId,
                text = "",
                emoji = null,
                ownKey = ownKey,
            )
            // The delete is out: the files go from the server now, or later
            // from the durable retry list.
            if (ownBlobs.isNotEmpty()) me.lakko.app.storage.BlobLifetime.delete(applicationContext, ownBlobs)
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
        add: Boolean = true,
    ): Unit =
        forSession {
            personAction(groupIdHex) {
                reactToMessageOnMlsThread(groupIdHex = groupIdHex, targetMessageId = targetMessageId, emoji = emoji, add = add)
            }
        }

    private suspend fun reactToMessageOnMlsThread(
        groupIdHex: String,
        targetMessageId: Long,
        emoji: String,
        add: Boolean,
    ): Unit {
        // ADR-0073 R.5 — a row of an older segment is read-only history: its
        // on-screen id is negative and names no message of this group.
        if (targetMessageId <= 0) return
        // ADR-0073 R.7 case 1 — an op on a message of a dead one-to-one is
        // applied on this device alone and never sent ([opRoute]): its target
        // exists only in the old session.
        if (deadOneToOne(groupIdHex)) {
            applyOwnOpLocally(groupIdHex, MessageOp.Reaction, targetMessageId, "", emoji, reactionAdd = add)
            return
        }
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        // reactions-and-emoji section 12 — the op names the target's author
        // (the account key this device holds for it), so the author's devices
        // can tell from the background that a reaction is to their message.
        val targetAuthor = record.messageAuthorKey(targetMessageId)?.let {
            try {
                java.util.Base64.getDecoder().decode(it).takeIf { b -> b.size == 32 }
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        // An add wakes the conversation like a message; taking one off stays
        // quiet.
        val wake = if (add) sealedWakeFor(groupIdHex, DsWakeKind.MESSAGE) else null
        flushOwedProfileBeforeSend(groupIdHex)
        groupLock(groupIdHex).sessionLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeReactionEnvelope(
                target = targetMessageId,
                emoji = emoji,
                action = if (add) uniffi.lakko_ffi_uniffi.ReactionAction.ADD else uniffi.lakko_ffi_uniffi.ReactionAction.REMOVE,
                targetAuthor = targetAuthor,
            )
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            // G-37 — provider work under the provider lock, publish outside it.
            val (prepared, ownKey) = providerLock.sessionLock {
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
            val sent = chatClient.publishPreparedSend(prepared, wake = wake)
            record.applyOwnOp(
                op = MessageOp.Reaction,
                target = targetMessageId,
                text = "",
                emoji = emoji,
                ownKey = ownKey,
                reactionAdd = add,
                opId = sent.messageId,
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
        forSession { sendTypingOnMlsThread(groupIdHex = groupIdHex, isTyping = isTyping) }

    private suspend fun sendTypingOnMlsThread(groupIdHex: String, isTyping: Boolean): Unit {
        // ADR-0073 R.7 case 1 — not sent into a dead one-to-one.
        if (deadOneToOne(groupIdHex)) return
        // ADR-0091 phase 2 section 2 — nor by a device behind there, nor
        // while a read of the journal was refused (walk 3, W3-R2).
        if (behindHoldsAutomaticSends(groupIdHex)) return
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        try {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeTypingEnvelope(
                isTyping = isTyping,
            )
            groupLock(groupIdHex).sessionLock {
                // SECURITY (nonce-reuse-on-crash) — a typing tick
                // still advances the MLS sender ratchet, so
                // persist the (N+1) state durably, fail-closed,
                // BEFORE publish. A persist failure aborts the
                // publish (no gen-N ciphertext on the wire) and is
                // swallowed below — a dropped typing tick is
                // invisible (the receiver auto-expires anyway).
                // G-37 — provider work under the provider lock, publish outside.
                val prepared = providerLock.sessionLock {
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
        forSession { sendReadReceiptOnMlsThread(groupIdHex = groupIdHex, upTo = upTo) }

    private suspend fun sendReadReceiptOnMlsThread(groupIdHex: String, upTo: Long): Unit {
        // ADR-0073 R.7 case 1 — not sent into a dead one-to-one.
        if (deadOneToOne(groupIdHex)) return
        // ADR-0091 phase 2 section 2 — nor by a device behind there, nor
        // while a read of the journal was refused (walk 3, W3-R2).
        if (behindHoldsAutomaticSends(groupIdHex)) return
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        try {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeReadReceiptEnvelope(
                upTo = upTo,
            )
            groupLock(groupIdHex).sessionLock {
                // SECURITY (nonce-reuse-on-crash) — a read-receipt
                // still advances the MLS sender ratchet, so
                // persist the (N+1) state durably, fail-closed,
                // BEFORE publish. A persist failure aborts the
                // publish (no gen-N ciphertext on the wire) and is
                // swallowed below — the next watermark advance
                // re-publishes.
                // G-37 — provider work under the provider lock, publish outside.
                val prepared = providerLock.sessionLock {
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

    /** ADR-0042 / ADR-0078 — send this device's sealed push-gw subscription
     *  for the group filed under [pushGid] ([PushSubscriptions.send]). */
    private suspend fun subscribePushGidBestEffort(pushGid: ByteArray) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        me.lakko.app.push.PushSubscriptions.sendForPushGid(applicationContext, accountIdB64, pushGid)
    }

    /**
     * ADR-0043 — drop this device's push-gw subscription under a SUPERSEDED
     * [pushGid], by its subscription id (ADR-0078).
     *
     * The endpoint deletes exactly the row named by its subscription id, and
     * a client knows only its OWN subscription ids — so this can only ever
     * retire THIS device's row, never a peer's.
     */
    private suspend fun unsubscribePushGidBestEffort(pushGid: ByteArray) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        me.lakko.app.push.PushSubscriptions.retire(applicationContext, accountIdB64, pushGid)
    }

    /**
     * ADR-0054 decision 1 with ADR-0078 section 3 — the wake a publish to
     * [groupIdHex] carries: the group's `push_gid`, the kind, a nonce, this
     * device's clock, the group's unlock key and a signature by the group's
     * wake key, sealed to push-gw's key ([me.lakko.app.push.PushWakeKey]), so
     * the DS forwards a blob it cannot read. `null` (no header) when [kind]
     * is `null`, when this device holds no routing secret for the group yet
     * (the ADR-0042 defer; the group id is never used in its place), or when
     * push-gw's key cannot be had.
     */
    private suspend fun sealedWakeFor(groupIdHex: String, kind: DsWakeKind?): DsWake? {
        if (kind == null) return null
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        val secret = PushRoutingSecretStore.get(applicationContext, accountIdB64, groupIdHex) ?: return null
        val gwKey = pushWakeKeyProvider() ?: return null
        return DsWake.seal(gwKey, secret, kind, System.currentTimeMillis())
    }

    /** push-gw's wake key; replaceable so a test supplies a fixed key. */
    internal var pushWakeKeyProvider: suspend () -> ByteArray? = { me.lakko.app.push.PushWakeKey.key() }

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
            // Each network leg ends by asking the session: what the routing
            // code does next is write this account's secret, route and
            // "distributed" rows, and a leg that returns after the account
            // left (or was forgotten) must not write them back.
            subscribeFn = { pushGid -> sessionLeg { subscribePushGidBestEffort(pushGid) } },
            unsubscribeFn = { pushGid -> sessionLeg { unsubscribePushGidBestEffort(pushGid) } },
            // ADR-0091 live walk 2026-11-19 — a skipped send (behind, or a
            // superseded one-to-one) answers null: it is not a send, so it
            // must leave the routing row pending rather than distributed.
            broadcastFn = { envelope ->
                sessionLeg {
                    me.lakko.app.push.requireRoutingBroadcastSent<SendMessageResult> {
                        broadcastControlToGroup(identity, groupIdHex, group, envelope)
                    }
                    Unit
                }
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
                providerLock.sessionLock { runCatching { group.epoch() }.getOrNull() }
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

    /**
     * ADR-0048 §2 — the own profile goes before this device's next message
     * into [groupIdHex] when one is owed there (a newcomer this device has
     * not yet named itself to, or a join publish that failed): published at
     * once and awaited, with no jitter, OUTSIDE the group lock it takes
     * itself. A failure does not hold the message back; it stays owed.
     */
    private suspend fun flushOwedProfileBeforeSend(groupIdHex: String) {
        try {
            profileRepublish.flushBeforeSend(groupIdHex)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0048 owed profile not flushed before a send")
        }
    }

    /** This account's own profile name, or `null` when none is set. The call
     *  host puts it on the user's own tile (`docs/ux/calls.md` section 4.4). */
    /** The name others see for this account, as a reply quote or a forward
     *  attribution carries it: the own display name, else `@username`; `null`
     *  when neither is set. */
    public fun ownSnapshotName(): String? {
        val accountId = keyPackageAccount.get()?.accountId ?: return null
        val own = me.lakko.app.profile.OwnProfileStore.load(applicationContext, accountId) ?: return null
        return own.displayName?.trim()?.takeIf { it.isNotEmpty() }
            ?: own.username?.trim()?.takeIf { it.isNotEmpty() }?.let { "@$it" }
    }

    public fun ownDisplayName(): String? {
        val accountId = keyPackageAccount.get()?.accountId ?: return null
        return me.lakko.app.profile.OwnProfileStore.load(applicationContext, accountId)?.displayName
    }

    /** ADR-0060 section 1 (2026-11-06) — whether the person holding a key was
     *  removed from the contacts (installed by the contacts repository). */
    @Volatile
    internal var contactGrantWithheldFor: ((peerKey: ByteArray) -> Boolean)? = null

    /** This account's own profile in wire form for [groupIdHex], or `null`
     *  when none is stored yet (nothing to publish). */
    private fun ownProfileForWire(groupIdHex: String, group: Group): uniffi.lakko_ffi_uniffi.FfiProfile? {
        val accountId = keyPackageAccount.get()?.accountId ?: return null
        // ADR-0060 §3 — a one-to-one carries `contact` + `member` and the
        // username; a group carries `member` always, and `group` + the username
        // only where the setting lets its members find this person.
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        val oneToOne = isOneToOne(group)
        val offered = reach.profileGrants(groupIdHex, oneToOne = oneToOne)
        // ADR-0060 section 1 — after Remove contact the removed person's own
        // one-to-one gets no contact grant, or the rotation hands them the new
        // key.
        val withheld = oneToOne && contactGrantWithheldFor?.let { removed ->
            val own = ownAccountKeysB64()
            runCatching { group.members() }.getOrNull().orEmpty()
                .map { it.accountKey }
                .filter { it.size == 32 && outboxB64(it) !in own }
                .any(removed)
        } == true
        val grants = offered.copy(grants = withoutContactGrant(offered.grants, withheld))
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
     *  be sent" (ADR-0074 3.5a rule 3 keeps only the second owed). */
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
            // Walk 3, W3-R2 — a held send (behind, or a refused read with no
            // answered read since) answers null: not sent, so the re-send
            // stays owed. Quiet.
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
            ) ?: return failed
            me.lakko.app.reach.ProfileResend.SENT
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0048 profile not published", cause)
            failed
        }
    }

    /** ADR-0074 3.5a rule 3 — the conversations that already took the
     *  profile re-sent under an account key (`"<keyHex>:<groupHex>"`), so a
     *  retry in this process does not send there again. In memory: after a
     *  restart an owed re-send goes to every conversation once more, which a
     *  receiver treats as the same profile. */
    private val profileResentTo: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** One re-send at a time. */
    private val profileResendMutex = kotlinx.coroutines.sync.Mutex()

    /**
     * ADR-0074 section 3.5a rule 3 — after the account's reach keys moved to
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
                android.util.Log.w("ChatRepository", "ADR-0074 3.5a: the profile re-send is still owed")
            }
            done
        }
    }

    /**
     * ADR-0076 - the account was renamed on another of its devices: the
     * certificate held here is the old name's. Drop it, fetch the current
     * one (`GET /discovery/username-certificate` answers for the current
     * name) and re-publish the profile, which the Everyone record and the
     * introduction record follow. Without a certificate the profile is still
     * re-published at once (the username line shows after the fetch).
     */
    public fun refreshUsernameCertificate() {
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        reach.dropUsernameCertificate()
        publishOwnProfileToAllGroups()
        val token = keyPackageAccount.get()?.sessionToken ?: return
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
                android.util.Log.i("ChatRepository", "ADR-0076: username certificate not fetched: ${cause.javaClass.simpleName}")
            }
        }
    }

    /**
     * ADR-0060 section 1 (2026-11-06) — a current contact wrote with a retired
     * `contact` key: the profile, carrying the current contact grant, goes
     * into the one-to-one with them. Nothing is shown. `false` when this
     * device holds no one-to-one with [peerKey].
     */
    public fun regrantContact(peerKey: ByteArray): Boolean {
        val hex = chatGroupIdHexFor(null, peerKey) ?: return false
        profileRepublish.publishNowToAll(listOf(hex))
        return true
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
        val account = keyPackageAccount.get()
        if (account == null) {
            // Before a session bootstrapped: the bootstrap refreshes.
            android.util.Log.i("ChatRepository", "ADR-0060 Everyone refresh waits for the session")
            return
        }
        reachScope.launchSession {
            if (!everyoneMutex.tryLock()) {
                everyoneAgain = true
                return@launchSession
            }
            try {
                do {
                    everyoneAgain = false
                    refreshEveryone(account)
                } while (everyoneAgain)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0060 Everyone publication failed: ${cause.javaClass.simpleName}")
                me.lakko.app.reach.ReachManager.get(applicationContext).noteEveryoneStuck()
            } finally {
                everyoneMutex.unlock()
            }
        }
    }

    /** One refresh at a time: the setting, the name and the note are read
     *  once for each send, so a run that started earlier can never overwrite
     *  the server with a choice the person has since changed. */
    private val everyoneMutex = kotlinx.coroutines.sync.Mutex()

    @Volatile
    private var everyoneAgain = false

    /** Set by the first Everyone refresh of a session
     *  ([me.lakko.app.reach.everyoneActionForRefresh]). */
    private val everyoneCheckedThisSession = java.util.concurrent.atomic.AtomicBoolean(false)

    private suspend fun refreshEveryone(account: KeyPackageAccount) {
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        val setting = reach.currentVisibility()
        if (setting == null) {
            android.util.Log.i("ChatRepository", "ADR-0060 Everyone refresh: no session bound to the setting")
            return
        }
        val everyone = setting.mode == uniffi.lakko_ffi_uniffi.FfiVisibilityMode.EVERYONE
        val own = me.lakko.app.profile.OwnProfileStore.load(applicationContext, account.accountId)
        val name = own?.displayName?.takeIf { it.isNotBlank() }
        var certificate = own?.username?.let { reach.usernameCertificate() }
        // The certificate is what publishes the username. A session that has
        // none yet (a restore, a rotation) fetches it here rather than leave
        // a person with no name out of the search.
        if (everyone && certificate == null && own?.username != null) {
            certificate = fetchUsernameCertificate(account, reach)
        }
        val key = me.lakko.app.reach.everyonePublishKey(name, own?.username, certificate, reach.ownMailboxBinding())
        val (published, publishedKey) = reach.publishedEveryone()
        val first = everyoneCheckedThisSession.compareAndSet(false, true)
        when (me.lakko.app.reach.everyoneActionForRefresh(everyone, published, publishedKey, key, first)) {
            me.lakko.app.reach.EveryoneAction.NONE -> return
            me.lakko.app.reach.EveryoneAction.NOTE_WITHDRAWN -> {
                // Never published: nothing to withdraw beyond the default.
                reach.notePublishedEveryone(false, null)
                return
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
                    mailboxBinding = reach.ownMailboxBinding(),
                ),
                ops = me.lakko.app.reach.EveryoneOps(
                    nameSearchTerms = { uniffi.lakko_ffi_uniffi.nameSearchTerms(it) },
                    oprfNameTerm = { t -> reach.keptOprf("n:$t") { client.oprfNameTerm(t) } },
                    oprfUsername = { u -> reach.keptOprf("u:$u") { client.oprfUsername(u) } },
                    composePublicRecord = { payload, outputs, issuedAt ->
                        uniffi.lakko_ffi_uniffi.composePublicRecord(payload, outputs, issuedAt)
                    },
                    putVisibility = { public, lookupKeys, record ->
                        client.putVisibility(public, lookupKeys, record)
                    },
                    // Publishing the own name: the terms not yet held, in one
                    // batch with one register token.
                    oprfOwnTerms = { own ->
                        val keys = own.map { t ->
                            when (t) {
                                is me.lakko.app.discovery.OwnTerm.Username -> "u:${t.value}"
                                is me.lakko.app.discovery.OwnTerm.NameTerm -> "n:${t.value}"
                            }
                        }
                        reach.keptOprfBatch(keys) { missing -> client.oprfOwnTerms(missing.map { own[it] }) }
                    },
                ),
                nowMs = System.currentTimeMillis(),
            )
        }
        // What was published is noted for the account that published
        // it; the reach manager may hold another one by now.
        ensureSession()
        when (outcome) {
            me.lakko.app.reach.EveryoneOutcome.PUBLISHED -> reach.notePublishedEveryone(true, key)
            me.lakko.app.reach.EveryoneOutcome.WITHDRAWN -> reach.notePublishedEveryone(false, null)
            me.lakko.app.reach.EveryoneOutcome.NOTHING_TO_PUBLISH -> {
                android.util.Log.w(
                    "ChatRepository",
                    "ADR-0060 Everyone not published: no name, no certified username or no identity",
                )
                reach.noteEveryoneStuck()
            }
        }
    }

    /** The username certificate, fetched and kept; `null` when there is none. */
    private suspend fun fetchUsernameCertificate(
        account: KeyPackageAccount,
        reach: me.lakko.app.reach.ReachManager,
    ): ByteArray? =
        try {
            val cert = keyPackageDiscoveryFactory().use { client ->
                client.setSessionToken(account.sessionToken)
                client.fetchUsernameCertificate()
            }
            cert?.also { reach.keepUsernameCertificate(it) }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.i("ChatRepository", "ADR-0060: username certificate not fetched: ${cause.javaClass.simpleName}")
            null
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
        reachScope.launchSession {
            if (!introductionMutex.tryLock()) {
                introductionsAgain = true
                return@launchSession
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
        val bound = callBinding()
        val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
        val setting = reach.currentVisibility() ?: return
        val fof = uniffi.lakko_ffi_uniffi.allowsContactsOfContacts(setting)
        val own = me.lakko.app.profile.OwnProfileStore.load(applicationContext, account.accountId)
        val username = own?.username?.takeIf { it.isNotEmpty() }
        val active = synchronized(groups) { groups.entries.toList() }
            .filter { (_, record) -> readIsActive { record.group.isActive() } != false }
        val oneToOnes = active.filter { (_, record) -> isOneToOne(record.group) }.map { it.key }.sorted()
        askForMissingIntroductions(active.map { it.key }.sorted(), identity, reach, bound)
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
                    offerGroups = me.lakko.app.reach.offerGroups(
                        form = setting.contactsForm,
                        oneToOneGroups = oneToOnes,
                        peerAccountId = { g -> oneToOnePeerKey(g)?.let { contactAccountId?.invoke(it) } },
                        allowsContact = { uniffi.lakko_ffi_uniffi.allowsContact(setting, it) },
                    ),
                    nowMs = System.currentTimeMillis(),
                ),
                ops = me.lakko.app.reach.IntroPublicationOps(
                    nameSearchTerms = { uniffi.lakko_ffi_uniffi.nameSearchTerms(it) },
                    oprfNameTerm = { client.oprfNameTerm(it) },
                    oprfUsername = { client.oprfUsername(it) },
                    oprfOwnTerms = { client.oprfOwnTerms(it) },
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
                        // Walk 3, W3-R2 — `null` is a held send: not recorded.
                        broadcastControlToGroup(
                            identity = identity,
                            groupIdHex = groupHex,
                            group = record.group,
                            envelope = envelope,
                        ) != null
                    },
                    save = {
                        // After the lookups and the sends: the publication
                        // state is the account's whose records went out.
                        bound.ensureCurrent()
                        reach.saveIntroPublicationState(it)
                    },
                ),
            )
        }
    }

    /**
     * ADR-0060 section 4, "A device that holds nothing asks": an offer and a
     * bundle go out once per conversation, so a device that missed them would
     * never get them. It asks with the field-less profile request, in the
     * store that request already uses: at most once a day, or once a week
     * while it holds a contact grant from that person.
     */
    private suspend fun askForMissingIntroductions(
        groupIds: List<String>,
        identity: Identity,
        reach: me.lakko.app.reach.ReachManager,
        bound: me.lakko.app.Binding,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        me.lakko.app.reach.askForIntroductions(
            heldByGroup = groupIds.associateWith { groupIdHex ->
                oneToOnePeerKey(groupIdHex)?.let(reach::introductionsHeldFrom)
            },
            decide = { groupIdHex, held ->
                me.lakko.app.reach.introductionsAskStamp(
                    held = held,
                    lastMs = profileRequestStore.lastRequestedAtMs(accountIdB64, groupIdHex),
                    nowMs = System.currentTimeMillis().toULong(),
                    noGrantRule = { last, now -> uniffi.lakko_ffi_uniffi.shouldRequestProfiles(false, 1u, last, now) },
                )
            },
            send = { groupIdHex ->
                val record = groups[groupIdHex] ?: throw IllegalStateException("conversation gone")
                broadcastControlToGroup(
                    identity = identity,
                    groupIdHex = groupIdHex,
                    group = record.group,
                    envelope = uniffi.lakko_ffi_uniffi.composeProfileRequest(),
                ) != null
            },
            record = { groupIdHex, stamp ->
                bound.ensureCurrent()
                profileRequestPolicy.recordSent(accountIdB64, groupIdHex, stamp)
            },
            noteFailure = { cause ->
                android.util.Log.w("ChatRepository", "ADR-0060 introductions ask not published", cause)
            },
        )
    }

    /** Start the periodic introductions re-check for this session. */
    private fun startIntroductionTicker() {
        introductionTicker?.cancel()
        introductionTicker = reachScope.launchSession {
            while (true) {
                kotlinx.coroutines.delay(INTRODUCTIONS_TICK_MS)
                runIntroductionsBestEffort()
                val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
                if (reach.everyonePublicationStuck()) refreshEveryoneBestEffort()
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
        reopenIntroductionsBestEffort(groupIdHex, oneToOne = isOneToOne(group), fromOtherPerson = own != null)
    }

    /**
     * ADR-0060 section 4 — the other person of a one-to-one asked: forget this
     * conversation in the sent records, so the offer and the bundle go out
     * again, at most once an hour. The profile answer above is what carries the
     * contact grant. Under the run's mutex: a run in progress holds a copy of
     * the state and would write these records back.
     */
    private fun reopenIntroductionsBestEffort(groupIdHex: String, oneToOne: Boolean, fromOtherPerson: Boolean) {
        if (!oneToOne || !fromOtherPerson) return
        reachScope.launchSession {
            val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
            val reopened = introductionMutex.sessionLock {
                val next = me.lakko.app.reach.reopenedByAsk(
                    state = reach.introPublicationState(),
                    groupHex = groupIdHex,
                    oneToOne = oneToOne,
                    fromOtherPerson = fromOtherPerson,
                    nowMs = System.currentTimeMillis(),
                ) ?: return@sessionLock false
                reach.saveIntroPublicationState(next)
                true
            }
            if (reopened) runIntroductionsBestEffort()
        }
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
        profileRequestScope.launchSession { requestProfilesBestEffort(groupIdHex, joinedJustNow = false) }
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
                isPlaceholder = { key -> profileDirectory.awaitsProfile(key) },
            )
        }
        val stamp = profileRequestPolicy.decide(
            accountIdB64 = accountIdB64,
            groupIdHex = groupIdHex,
            joinedJustNow = joinedJustNow,
            unnamedMembers = unnamed,
        ) ?: return
        try {
            // Walk 3, W3-R2 — a held send is not recorded as sent. Quiet.
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = uniffi.lakko_ffi_uniffi.composeProfileRequest(),
            ) ?: return
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0048 profile request not published", cause)
            return
        }
        ensureSession()
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
                // ADR-0048 v2 / ADR-0060 §5 / ADR-0084: the one conversion.
                profile = me.lakko.app.profile.peerProfileOf(profile),
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
            // ADR-0074 3.5a rule 2 — the roster names the keys the leaves are
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
                // ADR-0078: a grant whose mailbox binding does not verify
                // against the sender's account key (resolved at receipt,
                // C-68, then to its current key) would route writes to a
                // mailbox the sender does not read: not kept.
                if (!me.lakko.app.contacts.grantBindsTo(grant, reach.mailboxAccountKey(senderBytes), mailboxCrypto)) continue
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
                    if (me.lakko.app.reach.keepsCommitterDelegation(senderKey, personKeysOf(group.members()))) {
                        me.lakko.app.reach.ReachManager.get(applicationContext)
                            .keepDelegation(groupIdHex, senderBytes, bytes)
                    }
                }
                me.lakko.app.reach.ScopedEnvelopeKind.INTRODUCTION_OFFER,
                me.lakko.app.reach.ScopedEnvelopeKind.INTRODUCTIONS -> {
                    // Only from the other participant of a one-to-one.
                    val people = personKeysOf(group.members())
                    if (people.size == 2 && people.contains(senderKey) && senderKey != ownIdentity(group)) {
                        // Kept even when no activity set the sink yet: the
                        // receive cursor moves past this row, so a drop here
                        // was a loss for good.
                        (introductionSink ?: defaultIntroductionSink)(senderBytes, kind, bytes)
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
     *  (the contacts-of-contacts store). `null` until an activity sets it: the
     *  store's own keep ([defaultIntroductionSink]) runs then, never a drop. */
    @Volatile
    public var introductionSink: ((ByteArray, me.lakko.app.reach.ScopedEnvelopeKind, ByteArray) -> Unit)? = null

    /** The same keep the activity installs, for a row handled before one exists. */
    private val defaultIntroductionSink: (ByteArray, me.lakko.app.reach.ScopedEnvelopeKind, ByteArray) -> Unit =
        { sender, kind, bytes ->
            me.lakko.app.reach.IntroductionIntake.keepInBackground(
                reach = me.lakko.app.reach.ReachManager.get(applicationContext),
                senderKey = sender,
                kind = kind,
                bytes = bytes,
                then = { runIntroductionsBestEffort() },
            )
        }

    /** ADR-0048 §2 — republish on growth when the roster gained a member. */
    private fun republishProfileIfGrown(
        groupIdHex: String,
        previousMemberKeys: List<String>?,
        currentMemberKeys: List<String>?,
        selfEvicted: Boolean,
        ownAdd: Boolean = false,
    ) {
        if (selfEvicted) return
        if (me.lakko.app.profile.membershipGrew(previousMemberKeys, currentMemberKeys)) {
            profileRepublish.onGrowth(groupIdHex)
            // ADR-0051 §3 — the newcomer cannot read the earlier envelope, so
            // an owner / admin holding the name re-sends it (original stamp),
            // on the same throttled + jittered schedule. The device that
            // committed the add sends at once: the newcomer's list has no name
            // until it does, and the schedule exists for the other members.
            if (ownAdd) groupNameRepublish.publishNow(groupIdHex) else groupNameRepublish.onGrowth(groupIdHex)
            // ADR-0087 18.1 — and the photo, the reactions and the @name.
            if (ownAdd) groupSettingsRepublish.publishNow(groupIdHex) else groupSettingsRepublish.onGrowth(groupIdHex)
            // ADR-0052 decision 3 — and the shared pins, verbatim with their
            // original stamps, on the same schedule.
            pinRepublish.onGrowth(groupIdHex)
            // ADR-0059 section 6 — and this person's shared channel list,
            // verbatim with its original stamp, where the switch is on.
            sharedChannels.groupGrew(groupIdHex)
            // ADR-0092 section 3.9 — the committer of an admission hands every
            // open poll and checklist to the newcomer.
            if (ownAdd) {
                val added = (currentMemberKeys.orEmpty().toSet() - previousMemberKeys.orEmpty().toSet()).size
                handOverOpenItemsBestEffort(groupIdHex, added)
            }
        }
    }

    /**
     * ADR-0092 section 3.9 — after this device committed an admission that
     * added a leaf: post every open poll and checklist this device holds as
     * open-items envelopes (signed bodies, results or ring, ticks), then the
     * stored ballot rows the journal may no longer hold. Best effort: a
     * failure is logged; the newcomer then sees only what arrives later.
     */
    private fun handOverOpenItemsBestEffort(groupIdHex: String, addedLeaves: Int) {
        val record = groups[groupIdHex] ?: return
        ballotScope.launch {
            try {
                val now = System.currentTimeMillis()
                val open = record.snapshot().filter { m ->
                    m.messageId > 0 && !m.deleted && when (m.content) {
                        is uniffi.lakko_ffi_uniffi.MessageContent.Poll -> pollViewOf(m, record.groupId, null, now)?.closed == false
                        is uniffi.lakko_ffi_uniffi.MessageContent.Checklist -> checklistViewOf(m)?.closed == false
                        else -> false
                    }
                }
                if (!uniffi.lakko_ffi_uniffi.openItemsHandoverDue(addedLeaves.toUInt(), open.size.toUInt())) return@launch
                val items = open.mapNotNull { m -> openItemOf(m) }
                val envelopes = uniffi.lakko_ffi_uniffi.planOpenItemsRows(items)
                items.forEach { runCatching { it.close() } }
                for (envelope in envelopes) {
                    publishApplicationMessage(groupIdHex, envelope, me.lakko.app.calls.CallSignalingWake.NONE)
                }
                for (m in open) {
                    if (!m.content.isAnonymousPoll()) continue
                    val rows = m.contentExtras.ballots
                        .filter { uniffi.lakko_ffi_uniffi.ballotNeedsHandover(it.createdAtMs, now) }
                        .map { it.row }
                    if (rows.isEmpty()) continue
                    for (envelope in uniffi.lakko_ffi_uniffi.planPollBallotsRows(m.messageId, rows)) {
                        publishApplicationMessage(groupIdHex, envelope, me.lakko.app.calls.CallSignalingWake.NONE)
                    }
                }
            } catch (cause: Throwable) {
                me.lakko.app.rethrowIfCancelled(cause)
                android.util.Log.w("ChatRepository", "open-items hand-over not sent (${cause.javaClass.simpleName})")
            }
        }
    }

    /** The hand-over item of one open poll or checklist row, or `null`. */
    private fun openItemOf(m: ChatMessage): uniffi.lakko_ffi_uniffi.OpenItem? {
        val author = identityBytes(m.authorKey) ?: return null
        return try {
            when (val c = m.content) {
                is uniffi.lakko_ffi_uniffi.MessageContent.Poll -> {
                    val ring = m.contentExtras.ring?.let { uniffi.lakko_ffi_uniffi.PollRing.decode(it) }
                    val votes = if (ring != null) {
                        emptyList()
                    } else {
                        m.contentExtras.ops.filter { it.kind == ContentOpKind.PollVote }.mapNotNull { op ->
                            identityBytes(op.sender)?.let {
                                uniffi.lakko_ffi_uniffi.PollVoteRecord(op.opId, op.createdAtMs, it, op.choices ?: ByteArray(0))
                            }
                        }
                    }
                    try {
                        uniffi.lakko_ffi_uniffi.openItemForPoll(m.messageId, c.poll, author, ring, votes)
                    } finally {
                        ring?.close()
                    }
                }
                is uniffi.lakko_ffi_uniffi.MessageContent.Checklist -> {
                    val latest = m.contentExtras.ops
                        .filter { it.kind == ContentOpKind.EditChecklist && it.sender == m.authorKey }
                        .maxByOrNull { it.opId }
                        ?.let { (it.content as? uniffi.lakko_ffi_uniffi.MessageContent.Checklist)?.checklist }
                        ?: c.checklist
                    val state = uniffi.lakko_ffi_uniffi.foldChecklist(
                        c.checklist.body,
                        author,
                        m.messageId,
                        m.contentExtras.ops.filter { it.kind == ContentOpKind.EditChecklist }.mapNotNull { op ->
                            val body = (op.content as? uniffi.lakko_ffi_uniffi.MessageContent.Checklist)?.checklist?.body
                            identityBytes(op.sender)?.let { k -> body?.let { uniffi.lakko_ffi_uniffi.ChecklistEditRecord(op.opId, k, it) } }
                        },
                        m.contentExtras.ops.filter { it.kind == ContentOpKind.ChecklistAdd }.mapNotNull { op ->
                            identityBytes(op.sender)?.let { uniffi.lakko_ffi_uniffi.ChecklistAddRecord(op.opId, it, op.privileged, op.text) }
                        },
                        ticksOf(m),
                        emptyList(),
                    )
                    uniffi.lakko_ffi_uniffi.openItemForChecklist(m.messageId, latest, author, state, ticksOf(m))
                }
                else -> null
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "open item skipped (${cause.javaClass.simpleName})")
            null
        }
    }

    private fun ticksOf(m: ChatMessage): List<uniffi.lakko_ffi_uniffi.ChecklistTickRecord> =
        m.contentExtras.ops.filter { it.kind == ContentOpKind.ChecklistTick }.mapNotNull { op ->
            val k = identityBytes(op.sender) ?: return@mapNotNull null
            val item = op.item ?: return@mapNotNull null
            uniffi.lakko_ffi_uniffi.ChecklistTickRecord(op.opId, k, op.privileged, item, op.done ?: false)
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
                forSession {
                    // Walk 3, W3-R2 — `null` is a held send.
                    broadcastControlToGroup(
                        identity = identity,
                        groupIdHex = groupIdHex,
                        group = record.group,
                        envelope = envelope,
                    ) != null
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
    public fun sharedPins(groupIdHex: String): Map<Long, StoredPin> = foldedPins(groupIdHex).shared

    /** This device's personal pins in the conversation, `messageId -> pinnedAtMs`. */
    public fun personalPins(groupIdHex: String): Map<Long, ULong> = foldedPins(groupIdHex).personal

    /** ADR-0073 R.5 — the held segments of the conversation BEFORE
     *  [groupIdHex], oldest first. */
    private fun olderSegmentsOf(groupIdHex: String): List<String> {
        val segments = conversationSegments(groupIdHex)
        val index = segments.indexOfFirst { it.equals(groupIdHex, ignoreCase = true) }
        return if (index <= 0) emptyList() else segments.subList(0, index)
    }

    /**
     * ADR-0073 R.5 — the pins of the conversation as the screen of
     * [groupIdHex] shows them: this group's keyed by message id, an older
     * segment's by its on-screen history id ([pinsAcrossSegments]), so a pin
     * of an older segment stays in the bar and opens where it was.
     */
    private fun foldedPins(groupIdHex: String): FoldedPins<StoredPin, ULong> {
        val account = currentAccountIdB64.get()
        val older = olderSegmentsOf(groupIdHex)
        val segments = ArrayList<SegmentPins<StoredPin, ULong>>(older.size + 1)
        older.forEachIndexed { index, hex ->
            segments += SegmentPins(
                distanceFromHead = older.size - index,
                shared = PinStore.loadShared(applicationContext, account, hex),
                personal = PinStore.loadPersonal(applicationContext, account, hex),
            )
        }
        segments += SegmentPins(
            distanceFromHead = 0,
            shared = PinStore.loadShared(applicationContext, account, groupIdHex),
            personal = PinStore.loadPersonal(applicationContext, account, groupIdHex),
        )
        return pinsAcrossSegments(segments)
    }

    /** The stored pin the on-screen row [rowId] of [groupIdHex]'s screen
     *  stands for ([pinTargetOfRow]); `null` for an id no row can have. */
    private fun pinTargetFor(groupIdHex: String, rowId: Long): PinTarget? =
        pinTargetOfRow(rowId, groupIdHex, olderSegmentsOf(groupIdHex))

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
        // ADR-0073 R.5 — a row of an older segment can be unpinned, never
        // pinned; the personal unpin is written under the OLDER group.
        val target = pinTargetFor(groupIdHex, messageId) ?: return
        if (pinRoute(older = target.older, pinned = pinned, shared = false) == PinRoute.REFUSED) return
        val next = LinkedHashMap(PinStore.loadPersonal(applicationContext, accountIdB64, target.groupIdHex))
        if (pinned) next[target.messageId] = System.currentTimeMillis().toULong() else next.remove(target.messageId)
        PinStore.savePersonal(applicationContext, accountIdB64, target.groupIdHex, next)
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
        // ADR-0073 R.5 — an older segment is read-only on the wire: its
        // shared unpin is recorded on this device alone, with a fresh stamp
        // so a late re-send of the old pin cannot bring it back; a pin of an
        // older segment is not offered.
        val target = pinTargetFor(groupIdHex, messageId) ?: return
        when (pinRoute(older = target.older, pinned = pinned, shared = true)) {
            PinRoute.REFUSED -> return
            PinRoute.LOCAL_ONLY -> {
                groupLock(target.groupIdHex).withLock {
                    val shared = PinStore.loadShared(applicationContext, accountIdB64, target.groupIdHex)
                    val next = LinkedHashMap(shared)
                    next[target.messageId] = StoredPin(pinned = false, updatedAtMs = stamp)
                    PinStore.saveShared(applicationContext, accountIdB64, target.groupIdHex, next)
                }
                return
            }
            PinRoute.ORDINARY -> Unit
        }
        val op = uniffi.lakko_ffi_uniffi.FfiPinOp(messageId = messageId, pinned = pinned, updatedAtMs = stamp)
        val envelope = forSession { uniffi.lakko_ffi_uniffi.composePinEnvelope(op) }
        forSession {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
                automatic = false,
            )
        }
        // The local apply, under the same per-group lock a receiver holds, so
        // the stamp is compared against the current stored row (C-62).
        groupLock(groupIdHex).sessionLock {
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
                // Walk 3, W3-R2 — `null` is a held send: not sent.
                if (broadcastControlToGroup(
                        identity = identity,
                        groupIdHex = groupIdHex,
                        group = record.group,
                        envelope = envelope,
                    ) != null
                ) {
                    sent = true
                }
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
    /** ADR-0065 section 4 — told the SEGMENT group and the DS ids of its
     *  messages deleted for everyone, so a saved one leaves the saved list. */
    @Volatile
    public var onMessagesDeleted: ((segmentHex: String, messageIds: Set<Long>) -> Unit)? = null

    /**
     * ADR-0065 section 3 — the keep buckets of the pinned messages: one per
     * conversation this device holds, naming the files and thumbnails of the
     * messages pinned in it, for everyone or for me.
     */
    public fun pinnedKeepBuckets(): List<me.lakko.app.storage.KeepBucket> {
        val accountIdB64 = currentAccountIdB64.get() ?: return emptyList()
        return groups.entries.mapNotNull { (groupIdHex, record) ->
            try {
                val pinned = PinStore.loadShared(applicationContext, accountIdB64, groupIdHex).keys +
                    PinStore.loadPersonal(applicationContext, accountIdB64, groupIdHex).keys
                if (pinned.isEmpty()) return@mapNotNull null
                val ids = record.snapshot().asSequence()
                    .filter { it.messageId in pinned && !it.deleted }
                    .flatMap { messageBlobIds(it).asSequence() }
                    .toList()
                me.lakko.app.storage.KeepBucket(
                    key = "pins:" + groupIdHex.lowercase(),
                    blobIdsHex = me.lakko.app.storage.blobHexes(ids),
                )
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "pinned keep bucket not read", cause)
                null
            }
        }
    }

    /** The message [messageId] of the conversation [conversationHex] when
     *  this device holds it (the Saved screen's rows), else `null`. */
    public fun heldMessage(segmentHex: String, messageId: Long): ChatMessage? {
        // The conversation that holds the segment (through the lineage), and
        // the row in it: a saved message is named by its segment and DS id.
        val thread = conversationThreadOf(state.value.conversations, segmentHex.lowercase()) ?: return null
        return rowForSavedKey(thread, SavedKey(segmentHex.lowercase(), messageId))
    }

    /** The folded conversation that holds the group [segmentHex] (any of its
     *  segments), or `null` when this device does not know it. */
    public fun conversationHolding(segmentHex: String): ChatThread? =
        conversationThreadOf(state.value.conversations, segmentHex.lowercase())
            // A segment this device does not hold, but the lineage names: the
            // conversation it belongs to.
            ?: conversationThreadOf(state.value.conversations, conversationIdOf(segmentHex))

    private fun dropPinsOfDeletedMessages(groupIdHex: String, record: GroupRecord) {
        try {
            val deletedIds = record.snapshot().asSequence().filter { it.deleted && it.messageId > 0L }
                .map { it.messageId }.toHashSet()
            if (deletedIds.isNotEmpty()) onMessagesDeleted?.invoke(groupIdHex.lowercase(), deletedIds)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0065 saved of deleted messages not dropped", cause)
        }
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
        owePublicRecordRepublish(groupIdHex)
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
        val envelope = forSession {
            uniffi.lakko_ffi_uniffi.composeGroupNameEnvelope(
                uniffi.lakko_ffi_uniffi.FfiGroupName(name = name, updatedAtMs = stamp),
            )
        }
        val row = StoredGroupName(name = name.trim(), updatedAtMs = stamp)
        if (storeFirst) GroupNameStore.save(applicationContext, accountIdB64, groupIdHex, row)
        forSession {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
                automatic = false,
            )
        }
        if (!storeFirst) GroupNameStore.save(applicationContext, accountIdB64, groupIdHex, row)
        owePublicRecordRepublish(groupIdHex)
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
            // Walk 3, W3-R2 — `null` is a held send: not sent.
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
            ) != null
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0051 group name not re-sent", cause)
            false
        }
    }

    // -----------------------------------------------------------------
    // ADR-0087 — the group photo (18.1), the allowed reactions
    // (reactions-and-emoji section 9), the @name inside the group (11) and
    // the knock key (5).
    // -----------------------------------------------------------------

    /** The growth re-send of the photo, the reactions and the handle, on the
     *  group-name schedule (one per-group throttle + jitter). */
    private val groupSettingsRepublish: me.lakko.app.profile.ProfileRepublishScheduler =
        me.lakko.app.profile.ProfileRepublishScheduler(
            scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
            publish = { groupIdHex -> republishGroupSettingsIfHolder(groupIdHex) },
        )

    /**
     * Knock seeds this session already sent, by `groupHex:adminHex` (an
     * admin) or `groupHex:self` (this account's other devices): the newest
     * version sent and the recipient's devices then (walk 2, defect 6). Set
     * only after every broadcast of a send went out. In memory: a restart
     * sends every version once more, as before.
     */
    private val knockSeedsSent = java.util.concurrent.ConcurrentHashMap<String, KnockSeedSentMark>()

    private fun forgetKnockSeedMarks(groupIdHex: String) {
        knockSeedsSent.keys.removeAll { it.startsWith("$groupIdHex:") }
    }

    /** The group's stored photo, or `null`. */
    public fun groupPhoto(groupIdHex: String): StoredGroupPhoto? =
        GroupSettingsStore.photo(applicationContext, currentAccountIdB64.get(), groupIdHex)

    /** What [groupIdHex]'s messages take. A one-to-one always takes every
     *  reaction (reactions-and-emoji section 9). */
    public fun groupAllowedReactions(groupIdHex: String): AllowedReactions {
        val record = groups[groupIdHex] ?: return AllowedReactions.All
        val oneToOne = try {
            record.group.groupKind() == GroupKind.ONE_TO_ONE
        } catch (_: Throwable) {
            false
        }
        if (oneToOne) return AllowedReactions.All
        return GroupSettingsStore.reactions(applicationContext, currentAccountIdB64.get(), groupIdHex)?.reactions
            ?: AllowedReactions.All
    }

    /** The group's @name, or `null` while none is held or the one held is not
     *  verified (ADR-0087 section 3: fail closed, an unverifiable name is not
     *  shown). */
    public fun groupHandle(groupIdHex: String): StoredGroupHandle? =
        GroupSettingsStore.handle(applicationContext, currentAccountIdB64.get(), groupIdHex)?.takeIf { it.verified }

    /** When a handle verification last ran per group (in memory). */
    private val handleVerifyAtMs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    @Volatile
    private var handleCertKeyCache: ByteArray? = null

    /** The username / handle certificate verifying key: the build-time pin,
     *  else the well-known key fetched once (the ADR-0060 username path). */
    private suspend fun handleCertKey(): ByteArray? {
        handleCertKeyCache?.let { return it }
        val pinned = me.lakko.app.BuildConfig.LAKKO_USERNAME_CERT_KEY
        val key = if (pinned.isNotBlank()) {
            try {
                me.lakko.app.auth.base64UrlDecodeNoPad(pinned).takeIf { it.size == 32 }
            } catch (_: IllegalArgumentException) {
                null
            }
        } else {
            try {
                discovery.fetchUsernameCertKey()
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                null
            }
        }
        handleCertKeyCache = key
        return key
    }

    /**
     * ADR-0087 section 3 — check a received @name's certificate against
     * discovery's verifying key and the lookup key of the name it shows (one
     * `discovery-lookup` token), and that the certificate's write key is the
     * key that signed the in-group value. At most once a minute per group;
     * a name that cannot be checked stays hidden.
     */
    private suspend fun verifyGroupHandleIfDue(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val stored = GroupSettingsStore.handle(applicationContext, accountIdB64, groupIdHex) ?: return
        if (stored.verified) return
        val now = System.currentTimeMillis()
        val last = handleVerifyAtMs[groupIdHex]
        if (last != null && now - last < HANDLE_VERIFY_RETRY_MS) return
        handleVerifyAtMs[groupIdHex] = now
        val key = handleCertKey() ?: return
        val lookupKey = try {
            discovery.oprfUsername(stored.handle).lookupKey
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            return
        }
        val ok = try {
            val cert = uniffi.lakko_ffi_uniffi.verifyHandleBinding(stored.certificate, key, lookupKey)
            cert.writePk.contentEquals(
                stored.certificate.copyOfRange(HANDLE_CERT_WRITE_KEY_START, HANDLE_CERT_WRITE_KEY_END),
            )
        } catch (_: Throwable) {
            false
        }
        if (!ok) {
            android.util.Log.i("ChatRepository", "ADR-0087 conversation handle certificate refused")
            return
        }
        // Only if the row did not change while the check ran.
        val now2 = GroupSettingsStore.handle(applicationContext, accountIdB64, groupIdHex)
        if (now2 == stored) {
            GroupSettingsStore.saveHandle(applicationContext, accountIdB64, groupIdHex, stored.copy(verified = true))
            publishThreadList()
        }
    }

    /** A stamp strictly above [stored] and not below the clock. */
    private fun nextSettingStamp(stored: ULong?): ULong {
        val now = System.currentTimeMillis().toULong()
        return if (stored != null && stored >= now) stored + 1u else now
    }

    private fun receiveGroupSettingBestEffort(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
        kind: GroupSettingKind,
        bytes: ByteArray,
    ) {
        try {
            receiveGroupSetting(group, groupIdHex, senderKey, kind, bytes)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087 group setting ${kind.name} not applied", cause)
        }
    }

    /**
     * ADR-0087 — the receive gate of a group setting, inside the poll's locked
     * apply region (per-group lock + provider lock held), so the role-table
     * read, the stored-row read, the verdict and the write are one
     * read-decide-write against CURRENT state (C-62 sub-invariant 5).
     */
    private fun receiveGroupSetting(
        group: Group,
        groupIdHex: String,
        senderKey: MemberIdentity?,
        kind: GroupSettingKind,
        bytes: ByteArray,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val senderBytes = senderKey?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        val groupKind = try {
            group.groupKind()
        } catch (_: Throwable) {
            null
        }
        when (kind) {
            GroupSettingKind.PHOTO -> {
                if (!groupNameAppliesTo(groupKind)) return
                val decoded = try {
                    uniffi.lakko_ffi_uniffi.decodeGroupPhotoEnvelope(bytes)
                } catch (_: Throwable) {
                    return
                }
                val stored = GroupSettingsStore.photo(applicationContext, accountIdB64, groupIdHex)
                val incoming = StoredGroupPhoto(me.lakko.app.profile.avatarRefsOf(decoded.avatar), decoded.updatedAtMs)
                val verdict = ownerOrAdminVerdict(
                    senderKey = senderBytes,
                    owner = roleTableOwner(group),
                    hasStored = stored != null,
                    identical = stored == incoming,
                    newer = { uniffi.lakko_ffi_uniffi.groupPhotoIsNewer(incoming.updatedAtMs, stored!!.updatedAtMs) },
                )
                if (!verdict.accepts || verdict == GroupNameVerdict.ACCEPT_IDENTICAL) {
                    if (!verdict.accepts) android.util.Log.i("ChatRepository", "ADR-0087 group photo refused: $verdict")
                    return
                }
                GroupSettingsStore.savePhoto(applicationContext, accountIdB64, groupIdHex, incoming)
                deleteReplacedOwnGroupPhotoBlobs(accountIdB64, groupIdHex, incoming.avatar)
                // 4.5.4 — an admin's photo reaches the owner's device here.
                owePublicRecordRepublish(groupIdHex)
            }
            GroupSettingKind.REACTIONS -> {
                if (!groupNameAppliesTo(groupKind)) return
                val decoded = try {
                    uniffi.lakko_ffi_uniffi.decodeGroupReactionsEnvelope(bytes)
                } catch (_: Throwable) {
                    return
                }
                val stored = GroupSettingsStore.reactions(applicationContext, accountIdB64, groupIdHex)
                val incoming = StoredGroupReactions(allowedReactionsOf(decoded.reactions), decoded.updatedAtMs)
                val verdict = ownerOrAdminVerdict(
                    senderKey = senderBytes,
                    owner = roleTableOwner(group),
                    hasStored = stored != null,
                    identical = stored == incoming,
                    newer = { uniffi.lakko_ffi_uniffi.groupReactionsIsNewer(incoming.updatedAtMs, stored!!.updatedAtMs) },
                )
                if (!verdict.accepts || verdict == GroupNameVerdict.ACCEPT_IDENTICAL) {
                    if (!verdict.accepts) android.util.Log.i("ChatRepository", "ADR-0087 group reactions refused: $verdict")
                    return
                }
                GroupSettingsStore.saveReactions(applicationContext, accountIdB64, groupIdHex, incoming)
            }
            GroupSettingKind.HANDLE -> {
                // Section 11 — no role check: the handle key's signature over
                // the group id and the fields proves it, so any member relays
                // it. The key is the one the certificate names
                // (`version(1) ‖ lookup_key(64) ‖ write_pk(32) ‖ ...`).
                val decoded = try {
                    uniffi.lakko_ffi_uniffi.decodeConversationHandleEnvelope(bytes)
                } catch (_: Throwable) {
                    return
                }
                val cert = decoded.certificate
                if (cert.size < HANDLE_CERT_WRITE_KEY_END) return
                val writeKey = cert.copyOfRange(HANDLE_CERT_WRITE_KEY_START, HANDLE_CERT_WRITE_KEY_END)
                val groupId = try {
                    group.groupId()
                } catch (_: Throwable) {
                    return
                }
                try {
                    uniffi.lakko_ffi_uniffi.verifyConversationHandle(decoded, groupId, writeKey)
                } catch (_: Throwable) {
                    android.util.Log.i("ChatRepository", "ADR-0087 conversation handle refused: signature")
                    return
                }
                val stored = GroupSettingsStore.handle(applicationContext, accountIdB64, groupIdHex)
                if (stored != null &&
                    !uniffi.lakko_ffi_uniffi.conversationHandleIsNewer(decoded.updatedAtMs, stored.updatedAtMs)
                ) {
                    return
                }
                // A re-sent name with the same certificate keeps its check.
                val sameCert = stored != null && stored.verified && stored.handle == decoded.handle &&
                    stored.certificate.contentEquals(decoded.certificate)
                GroupSettingsStore.saveHandle(
                    applicationContext,
                    accountIdB64,
                    groupIdHex,
                    StoredGroupHandle(
                        handle = decoded.handle,
                        public = decoded.public,
                        updatedAtMs = decoded.updatedAtMs,
                        certificate = decoded.certificate,
                        signature = decoded.signature,
                        verified = sameCert,
                    ),
                )
                if (!sameCert) {
                    handleVerifyAtMs.remove(groupIdHex)
                    pushScope.launchSession {
                        try {
                            verifyGroupHandleIfDue(groupIdHex)
                        } catch (cause: kotlinx.coroutines.CancellationException) {
                            throw cause
                        } catch (cause: Throwable) {
                            android.util.Log.w("ChatRepository", "ADR-0087 handle verify failed", cause)
                        }
                    }
                }
                publishThreadList()
            }
            GroupSettingKind.KNOCK_SEED -> {
                // Section 5 — the owner hands one admin `knock_seed(v)`, sealed
                // to that admin's account key. Kept only when addressed to this
                // account and sent by the live role table's owner (C-68).
                val decoded = try {
                    uniffi.lakko_ffi_uniffi.decodeKnockSeedEnvelope(bytes)
                } catch (_: Throwable) {
                    return
                }
                val groupId = try {
                    group.groupId()
                } catch (_: Throwable) {
                    return
                }
                if (!decoded.journalId.contentEquals(groupId)) return
                val own = ownAccountKey() ?: return
                if (!decoded.memberAccountKey.contentEquals(own)) return
                val owner = roleTableOrNull(group)?.owner ?: return
                if (senderBytes == null || !owner.contentEquals(senderBytes)) return
                val account = accountIdentity.get() ?: return
                val seed = try {
                    uniffi.lakko_ffi_uniffi.openKnockSeedEnvelope(bytes, account)
                } catch (_: Throwable) {
                    return
                }
                try {
                    // The first seed given for a version is kept: a device of
                    // the owner that derived a version from the wrong root
                    // (before it learned of a rotation) cannot replace it.
                    val held = GroupSettingsStore.knockSeed(applicationContext, accountIdB64, groupIdHex, decoded.knockVersion)
                    if (held == null) {
                        GroupSettingsStore.saveKnockSeed(applicationContext, accountIdB64, groupIdHex, decoded.knockVersion, seed)
                    } else {
                        held.fill(0)
                    }
                    // Section 5 — a newer version is the one new links carry.
                    if (decoded.knockVersion > GroupSettingsStore.knockVersion(applicationContext, accountIdB64, groupIdHex)) {
                        GroupSettingsStore.saveKnockVersion(applicationContext, accountIdB64, groupIdHex, decoded.knockVersion)
                    }
                } finally {
                    seed.fill(0)
                }
            }
        }
    }

    /**
     * ADR-0087 18.1 — set (or with a `null` [avatar] remove) the group photo:
     * compose, publish into the group, then store. Owner or admin only (the
     * peers refuse anyone else); throws on a refused value or a failed send.
     */
    public suspend fun setGroupPhoto(groupIdHex: String, avatar: me.lakko.app.profile.AvatarRefs?) {
        val identity = canonicalIdentity.get() ?: throw IllegalStateException("not bootstrapped")
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        val accountIdB64 = currentAccountIdB64.get() ?: throw IllegalStateException("no session")
        // 4.5.2 — demoted while the crop screen was open: nothing is sent.
        if (!mayChangeGroupSettings(groupIdHex)) throw GroupPhotoNotAllowed()
        val ffi = avatar?.let { it.toFfi() ?: throw IllegalStateException("malformed photo") }
        val stored = groupPhoto(groupIdHex)
        val stamp = nextSettingStamp(stored?.updatedAtMs)
        val envelope = forSession {
            uniffi.lakko_ffi_uniffi.composeGroupPhotoEnvelope(
                uniffi.lakko_ffi_uniffi.FfiGroupPhoto(avatar = ffi, updatedAtMs = stamp),
            )
        }
        try {
            forSession { broadcastControlToGroup(identity, groupIdHex, record.group, envelope, automatic = false) }
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException || !isNetworkFailure(cause)) throw cause
            // 4.5.2 — uploaded, the device went offline: kept owed and sent
            // when the device is back, also after a restart.
            GroupSettingsStore.saveOwedPhoto(applicationContext, accountIdB64, groupIdHex, StoredGroupPhoto(avatar, stamp))
            throw GroupPhotoOwed()
        }
        GroupSettingsStore.saveOwedPhoto(applicationContext, accountIdB64, groupIdHex, null)
        GroupSettingsStore.savePhoto(applicationContext, accountIdB64, groupIdHex, StoredGroupPhoto(avatar, stamp))
        if (avatar != null) {
            val own = GroupSettingsStore.owedDeletes(applicationContext, accountIdB64, groupIdHex)
            val mine = listOfNotNull(avatar.still, avatar.video).map { it.blobIdHex }
            GroupSettingsStore.saveOwedDeletes(applicationContext, accountIdB64, groupIdHex, own + mine)
        }
        deleteReplacedOwnGroupPhotoBlobs(accountIdB64, groupIdHex, avatar)
        owePublicRecordRepublish(groupIdHex)
        publishThreadList()
    }

    /** Owner or admin of the group (or a group with no role table): may set
     *  the photo, the reactions and the name. */
    private suspend fun mayChangeGroupSettings(groupIdHex: String): Boolean {
        val identity = canonicalIdentity.get() ?: return false
        val record = groups[groupIdHex] ?: return false
        return providerLock.sessionLock {
            try {
                when (record.group.ownRole(identity)) {
                    Role.OWNER, Role.ADMIN -> true
                    Role.MEMBER -> false
                    null -> record.group.groupRoles() == null
                }
            } catch (_: Throwable) {
                false
            }
        }
    }

    private fun isNetworkFailure(cause: Throwable): Boolean {
        var c: Throwable? = cause
        val seen = HashSet<Throwable>()
        while (c != null && seen.add(c)) {
            if (c is me.lakko.app.ds.DsNetworkError || c is java.io.IOException) return true
            c = c.cause
        }
        return false
    }

    /** 4.5.2 — send an owed photo once the device is back. Best effort. */
    private suspend fun sendOwedGroupPhotoIfAny(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val owed = GroupSettingsStore.owedPhoto(applicationContext, accountIdB64, groupIdHex) ?: return
        try {
            setGroupPhoto(groupIdHex, owed.avatar)
        } catch (_: GroupPhotoNotAllowed) {
            GroupSettingsStore.saveOwedPhoto(applicationContext, accountIdB64, groupIdHex, null)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            // Still owed.
        }
    }

    /** The photo shown while it is owed, for the header. */
    public fun owedGroupPhoto(groupIdHex: String): StoredGroupPhoto? =
        GroupSettingsStore.owedPhoto(applicationContext, currentAccountIdB64.get(), groupIdHex)

    /**
     * ADR-0087 18.5 — a photo this account uploaded that is no longer the
     * group's goes from the server. The record of own uploads is this
     * account's; [me.lakko.app.storage.BlobLifetime] keeps the delete owed
     * and retries it.
     */
    private fun deleteReplacedOwnGroupPhotoBlobs(
        accountIdB64: String,
        groupIdHex: String,
        current: me.lakko.app.profile.AvatarRefs?,
    ) {
        val own = GroupSettingsStore.owedDeletes(applicationContext, accountIdB64, groupIdHex)
        if (own.isEmpty()) return
        val keep = listOfNotNull(current?.still, current?.video).map { it.blobIdHex }.toSet()
        val gone = own.filter { it !in keep }
        if (gone.isEmpty()) return
        val ids = gone.mapNotNull { hexToBytes(it)?.takeIf { b -> b.size == 32 } }
        try {
            if (ids.isNotEmpty()) me.lakko.app.storage.BlobLifetime.delete(applicationContext, ids)
        } finally {
            GroupSettingsStore.saveOwedDeletes(applicationContext, accountIdB64, groupIdHex, own.filter { it in keep })
        }
    }

    /** ADR-0065 / ADR-0087 18.5 — the blob ids of the photos of the groups
     *  this device owns or administers, for the keep tick. */
    public suspend fun conversationPhotoBlobIds(): List<ByteArray> {
        val accountIdB64 = currentAccountIdB64.get() ?: return emptyList()
        val identity = canonicalIdentity.get() ?: return emptyList()
        val out = ArrayList<ByteArray>()
        for (groupIdHex in GroupSettingsStore.photoGroups(applicationContext, accountIdB64)) {
            val record = groups[groupIdHex] ?: continue
            val keeps = providerLock.sessionLock {
                try {
                    val role = record.group.ownRole(identity)
                    role == Role.OWNER || role == Role.ADMIN
                } catch (_: Throwable) {
                    false
                }
            }
            if (!keeps) continue
            out += me.lakko.app.profile.avatarBlobIds(groupPhoto(groupIdHex)?.avatar)
        }
        return out
    }

    /**
     * Reactions-and-emoji section 9 — set what the group's messages take.
     * Owner or admin only; throws on a refused value or a failed send.
     */
    public suspend fun setGroupReactions(groupIdHex: String, reactions: AllowedReactions) {
        val identity = canonicalIdentity.get() ?: throw IllegalStateException("not bootstrapped")
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        val accountIdB64 = currentAccountIdB64.get() ?: throw IllegalStateException("no session")
        val ffi = reactions.toFfiAllowed()
        uniffi.lakko_ffi_uniffi.validateAllowedReactions(ffi)
        val stored = GroupSettingsStore.reactions(applicationContext, accountIdB64, groupIdHex)
        val stamp = nextSettingStamp(stored?.updatedAtMs)
        val envelope = forSession {
            uniffi.lakko_ffi_uniffi.composeGroupReactionsEnvelope(
                uniffi.lakko_ffi_uniffi.FfiGroupReactions(reactions = ffi, updatedAtMs = stamp),
            )
        }
        forSession { broadcastControlToGroup(identity, groupIdHex, record.group, envelope, automatic = false) }
        GroupSettingsStore.saveReactions(applicationContext, accountIdB64, groupIdHex, StoredGroupReactions(reactions, stamp))
        publishThreadList()
    }

    /**
     * The growth re-send: an owner / admin re-sends the stored photo and
     * reactions with their ORIGINAL stamps; any member relays the stored
     * handle verbatim (its signature is the owner's).
     */
    private suspend fun republishGroupSettingsIfHolder(groupIdHex: String): Boolean {
        val identity = canonicalIdentity.get() ?: return false
        val record = groups[groupIdHex] ?: return false
        if (readIsActive { record.group.isActive() } == false) return false
        val accountIdB64 = currentAccountIdB64.get() ?: return false
        val mayResend = try {
            when (record.group.ownRole(identity)) {
                Role.OWNER, Role.ADMIN -> true
                Role.MEMBER -> false
                null -> record.group.groupRoles() == null
            }
        } catch (_: Throwable) {
            false
        }
        val envelopes = ArrayList<ByteArray>()
        if (mayResend) {
            GroupSettingsStore.photo(applicationContext, accountIdB64, groupIdHex)?.let { p ->
                val ffi = p.avatar?.toFfi()
                if (p.avatar == null || ffi != null) {
                    runCatching {
                        uniffi.lakko_ffi_uniffi.composeGroupPhotoEnvelope(
                            uniffi.lakko_ffi_uniffi.FfiGroupPhoto(avatar = ffi, updatedAtMs = p.updatedAtMs),
                        )
                    }.getOrNull()?.let(envelopes::add)
                }
            }
            GroupSettingsStore.reactions(applicationContext, accountIdB64, groupIdHex)?.let { r ->
                runCatching {
                    uniffi.lakko_ffi_uniffi.composeGroupReactionsEnvelope(
                        uniffi.lakko_ffi_uniffi.FfiGroupReactions(reactions = r.reactions.toFfiAllowed(), updatedAtMs = r.updatedAtMs),
                    )
                }.getOrNull()?.let(envelopes::add)
            }
        }
        GroupSettingsStore.handle(applicationContext, accountIdB64, groupIdHex)?.let { h ->
            runCatching { uniffi.lakko_ffi_uniffi.composeConversationHandleEnvelope(h.toFfi()) }
                .getOrNull()?.let(envelopes::add)
        }
        if (envelopes.isEmpty()) return false
        var sent = false
        for (envelope in envelopes) {
            try {
                // Walk 3, W3-R2 — `null` is a held send: not sent.
                if (broadcastControlToGroup(identity, groupIdHex, record.group, envelope) != null) sent = true
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0087 group setting not re-sent", cause)
            }
        }
        return sent
    }

    /**
     * ADR-0087 — whether this account is the group's owner, for the knock key,
     * links and handle writes. The role table names the owner by an account
     * key; after an ADR-0074 rotation that may be the conversation key this
     * device's leaf is bound under rather than the current identity key, so
     * every key this account answers to is accepted. Provider critical path.
     */
    private fun ownsGroupAsOwner(group: Group): Boolean {
        val owner = roleTableOrNull(group)?.owner ?: return false
        val candidates = listOfNotNull(
            ownAccountKey(),
            try {
                publishedAccountIdentity.get()?.signaturePublicKey()
            } catch (_: Throwable) {
                null
            },
            ownIdentity(group)?.let {
                try {
                    java.util.Base64.getDecoder().decode(it)
                } catch (_: IllegalArgumentException) {
                    null
                }
            },
        )
        return candidates.any { it.contentEquals(owner) }
    }

    /** Section 5 — the knock seed of [version] this device can read rows
     *  with: derived when this account owns the journal, else the one the
     *  owner handed this admin. The caller wipes it. */
    private fun knockSeedFor(record: GroupRecord, groupIdHex: String, version: UShort): ByteArray? {
        // A stored seed first: one the owner handed this admin, or a version
        // derived from a predecessor's root before an ADR-0074 rotation moved
        // the root (section 5).
        GroupSettingsStore.knockSeed(applicationContext, currentAccountIdB64.get(), groupIdHex, version)?.let { return it }
        if (ownsGroupAsOwner(record.group)) {
            val account = rootAccountIdentity() ?: return null
            val root = try {
                uniffi.lakko_ffi_uniffi.dsJournalRootSeed(account.ed25519Seed(), record.groupId)
            } catch (_: Throwable) {
                return null
            }
            return try {
                uniffi.lakko_ffi_uniffi.groupKnockSeed(root, version)
            } catch (_: Throwable) {
                null
            } finally {
                root.fill(0)
            }
        }
        return null
    }

    /** Section 5 — the knock key a link names (the group's current version),
     *  or with [version] a given one; `null` when this device holds no seed. */
    private fun groupKnockKeyFor(
        record: GroupRecord,
        groupIdHex: String,
        version: UShort = GroupSettingsStore.knockVersion(applicationContext, currentAccountIdB64.get(), groupIdHex),
    ): Pair<ByteArray, UShort>? {
        val seed = knockSeedFor(record, groupIdHex, version) ?: return null
        return try {
            uniffi.lakko_ffi_uniffi.groupKnockPublicKey(seed) to version
        } catch (_: Throwable) {
            null
        } finally {
            seed.fill(0)
        }
    }

    /** Section 5 — the owner hands each admin the knock seeds it was not yet
     *  sent this session (the receiver keeps them; a repeat is harmless). */
    private suspend fun deliverKnockSeedsBestEffort(
        identity: Identity,
        record: GroupRecord,
        groupIdHex: String,
        admins: List<ByteArray>,
    ) {
        val own = ownAccountKey() ?: return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        // Section 5 — removing an admin bumps the version: the owner hands the
        // next seed only to the admins who remain, and new links carry it.
        // Older versions stay derivable, so requests sealed to them still open.
        val current = admins.filter { !it.contentEquals(own) }.map { me.lakko.app.ds.hexOf(it) }.toSet()
        val held = GroupSettingsStore.knockVersion(applicationContext, accountIdB64, groupIdHex)
        val version = nextKnockVersion(GroupSettingsStore.knockAdmins(applicationContext, accountIdB64, groupIdHex), current, held)
        if (version != held) {
            GroupSettingsStore.saveKnockVersion(applicationContext, accountIdB64, groupIdHex, version)
            // Walk 2 (defect 6) — the marks stay: each recipient is sent only
            // the versions it was not sent ([knockSeedVersionsToSend]).
            // The account's other devices learn the bump from the seeds this
            // device addresses to its own account (owed until sent).
            GroupSettingsStore.saveKnockSelfOwed(applicationContext, accountIdB64, groupIdHex, true)
            // The public record is republished after a bump: recorded owed now,
            // durably, and sent by the handle maintenance until it lands.
            GroupSettingsStore.handle(applicationContext, accountIdB64, groupIdHex)?.takeIf { it.public }?.let { h ->
                if (GroupSettingsStore.owedTypeChange(applicationContext, accountIdB64, groupIdHex) == null) {
                    GroupSettingsStore.saveOwedTypeChange(applicationContext, accountIdB64, groupIdHex, true to h.handle)
                }
            }
        }
        GroupSettingsStore.saveKnockAdmins(applicationContext, accountIdB64, groupIdHex, current)
        // Every version up to the current one reaches each admin, so rows from
        // older links (and from before a rotation) still open on its device;
        // a removed admin gets nothing new. Walk 2 (defect 6): only the
        // versions a recipient was not yet sent in this session, unless it
        // has a device that was not there then. Before, every version went
        // to every admin after each bump and again to this account, one
        // in-group message each, so a role change cost one message per
        // admin removal the group ever saw.
        val devicesOf: Map<String, Set<String>> = try {
            providerLock.sessionLock {
                record.group.members().groupBy(
                    { me.lakko.app.ds.hexOf(it.accountKey) },
                    { me.lakko.app.ds.hexOf(it.signaturePublicKey) },
                ).mapValues { it.value.toSet() }
            }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            return
        }
        val ownHex = me.lakko.app.ds.hexOf(own)
        // Section 5 — after a bump or a rotation on THIS device, the owner's
        // other devices are told: the seeds it holds, sealed to the own
        // account key. A receiving device of the account keeps them as given
        // seeds and raises its current version from them.
        if (GroupSettingsStore.knockSelfOwed(applicationContext, accountIdB64, groupIdHex)) {
            val tag = "$groupIdHex:self"
            val ownDevices = devicesOf[ownHex].orEmpty()
            try {
                val x25519 = accountIdentity.get()?.x25519PublicKey()
                if (x25519 != null) {
                    val versions = knockSeedVersionsToSend(knockSeedsSent[tag], version.toInt(), ownDevices)
                    var allSent = true
                    for (v in versions) {
                        val seed = knockSeedFor(record, groupIdHex, v.toUShort()) ?: continue
                        val envelope = try {
                            uniffi.lakko_ffi_uniffi.composeKnockSeedEnvelope(record.groupId, own, x25519, v.toUShort(), seed)
                        } finally {
                            seed.fill(0)
                        }
                        // `null` is a skipped send (behind, a dead one-to-one):
                        // never taken as sent.
                        if (broadcastControlToGroup(identity, groupIdHex, record.group, envelope) == null) {
                            allSent = false
                            break
                        }
                    }
                    if (allSent) {
                        knockSeedsSent[tag] = KnockSeedSentMark(version.toInt(), ownDevices)
                        GroupSettingsStore.saveKnockSelfOwed(applicationContext, accountIdB64, groupIdHex, false)
                    }
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0087 own knock seeds not delivered; still owed (${errorLogTag(cause)})")
            }
        }
        for (admin in admins) {
            if (admin.contentEquals(own)) continue
            val adminHex = me.lakko.app.ds.hexOf(admin)
            val tag = "$groupIdHex:$adminHex"
            val adminDevices = devicesOf[adminHex].orEmpty()
            val versions = knockSeedVersionsToSend(knockSeedsSent[tag], version.toInt(), adminDevices)
            if (versions.isEmpty()) continue
            try {
                val x25519 = memberX25519KeyOrNull(admin) ?: continue
                var allSent = true
                for (v in versions) {
                    val seed = knockSeedFor(record, groupIdHex, v.toUShort()) ?: continue
                    val envelope = try {
                        uniffi.lakko_ffi_uniffi.composeKnockSeedEnvelope(
                            record.groupId,
                            admin,
                            x25519,
                            v.toUShort(),
                            seed,
                        )
                    } finally {
                        seed.fill(0)
                    }
                    if (broadcastControlToGroup(identity, groupIdHex, record.group, envelope) == null) {
                        allSent = false
                        break
                    }
                }
                if (allSent) knockSeedsSent[tag] = KnockSeedSentMark(version.toInt(), adminDevices)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0087 knock seed not delivered (${errorLogTag(cause)})")
            }
        }
    }

    /** What one sealed knock row opened to. */
    private class OpenedKnock(
        val joinerSignatureKey: ByteArray,
        val keyPackage: ByteArray,
        val replyGrant: uniffi.lakko_ffi_uniffi.FfiReachGrant,
        /** The `/g/` link's issuer key when the request came by a link. */
        val linkIssuer: ByteArray? = null,
    )

    /**
     * Section 5 — open one sealed knock row and keep its KeyPackage as a
     * request to join. A private group takes a row only with a capability of
     * this group; a full queue stops the cursor before the row.
     */
    private fun openSealedKnock(
        record: GroupRecord,
        groupIdHex: String,
        identity: Identity,
        bytes: ByteArray,
    ): Pair<SealedKnockStep, OpenedKnock?> {
        val version = uniffi.lakko_ffi_uniffi.knockRowVersion(bytes) ?: return SealedKnockStep.DROP to null
        val seed = knockSeedFor(record, groupIdHex, version) ?: return SealedKnockStep.DROP to null
        val request = try {
            uniffi.lakko_ffi_uniffi.openKnockRow(seed, bytes)
        } catch (_: Throwable) {
            // ADR-0091 phase 2 route 3 — a member's request to be brought
            // back, sealed to the same key under its own AAD: handed to the
            // re-entry path, never kept as a request to join.
            openReentryKnockRowOrNull(record, groupIdHex, bytes)?.let {
                receiveConversationReentry(it, ReentryRouteKind.KNOCK)
            }
            return SealedKnockStep.DROP to null
        } finally {
            seed.fill(0)
        }
        val capability = request.capability
        val linkIssuer: ByteArray? = capability?.let {
            try {
                uniffi.lakko_ffi_uniffi.GroupInviteCapability.decodeAndVerify(it).use { cap ->
                    cap.issuerSignaturePubkey().takeIf { cap.groupId().contentEquals(record.groupId) }
                }
            } catch (_: Throwable) {
                null
            }
        }
        val byLink = linkIssuer != null
        val public = groupHandle(groupIdHex)?.public == true
        if (!byLink && !public) return SealedKnockStep.DROP to null
        val kept = try {
            record.group.retainJoinRequest(identity, request.keyPackage)
        } catch (_: Throwable) {
            return SealedKnockStep.DROP to null
        }
        return when (kept.outcome) {
            uniffi.lakko_ffi_uniffi.FfiJoinRequestOutcome.RETAINED,
            uniffi.lakko_ffi_uniffi.FfiJoinRequestOutcome.ALREADY_PENDING,
            -> SealedKnockStep.KEEP to OpenedKnock(kept.joinerSignatureKey, request.keyPackage, request.replyGrant, linkIssuer)
            uniffi.lakko_ffi_uniffi.FfiJoinRequestOutcome.QUEUE_FULL -> SealedKnockStep.STOP to null
            uniffi.lakko_ffi_uniffi.FfiJoinRequestOutcome.ALREADY_MEMBER,
            uniffi.lakko_ffi_uniffi.FfiJoinRequestOutcome.NOT_ADMISSIBLE,
            -> SealedKnockStep.DROP to null
        }
    }

    /** ADR-0060 section 6 — keep the reply key a knock carried for the
     *  joiner's account, so the sweep can write the Welcome into its closed
     *  mailbox. Awaited: the knock position is saved only after it is kept.
     *  The grant is also recorded per group, so it is cleared when the
     *  request is refused ([releaseKnockReply]). */
    private suspend fun keepKnockReplyGrant(groupIdHex: String, opened: OpenedKnock) {
        if (opened.replyGrant.`class` != uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY) return
        val account = try {
            uniffi.lakko_ffi_uniffi.keyPackageAccountKey(opened.keyPackage)
        } catch (_: Throwable) {
            null
        } ?: opened.joinerSignatureKey
        if (!me.lakko.app.contacts.grantBindsTo(opened.replyGrant, account, mailboxCrypto)) return
        me.lakko.app.reach.ReachManager.get(applicationContext).fileHeld(account, opened.replyGrant, scopeHex = "")
        recordKnockReply(groupIdHex, account, opened.replyGrant)
    }

    /** Record a join request's reply grant for [account] in [groupIdHex], so
     *  it is cleared when the request is refused. */
    private fun recordKnockReply(groupIdHex: String, account: ByteArray, grant: uniffi.lakko_ffi_uniffi.FfiReachGrant) {
        val acct = currentAccountIdB64.get() ?: return
        try {
            GroupSettingsStore.saveKnockReply(
                applicationContext,
                acct,
                groupIdHex,
                me.lakko.app.ds.hexOf(account),
                me.lakko.app.reach.reachHex(uniffi.lakko_ffi_uniffi.encodeReachGrant(grant)),
            )
        } catch (_: Throwable) {
            // The grant is kept; only its release on refuse is lost.
        }
    }

    /**
     * group-and-channel-privacy.md section 8 / ADR-0060 §6 — the person who
     * asked to join [groupIdHex] was refused: the reply grant their request
     * carried is no longer needed and is dropped. Best effort. An admitted
     * person's grant is kept until it expires, for a Welcome re-sent later.
     */
    private suspend fun releaseKnockReply(groupIdHex: String, account: ByteArray) {
        val acct = currentAccountIdB64.get() ?: return
        val accountHex = me.lakko.app.ds.hexOf(account)
        try {
            val wireHex = GroupSettingsStore.knockReply(applicationContext, acct, groupIdHex, accountHex) ?: return
            val wire = me.lakko.app.reach.reachUnhex(wireHex)
            if (wire != null) {
                me.lakko.app.reach.ReachManager.get(applicationContext)
                    .withdrawHeldGrant(account, me.lakko.app.reach.ReachKind.REPLY, wire)
            }
            GroupSettingsStore.saveKnockReply(applicationContext, acct, groupIdHex, accountHex, null)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087 a join reply grant not released: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * ADR-0091 phase 1b — the poll's fetch half: when the group was loaded
     * below the epoch recorded with its cursor and the DS names the commit
     * leaving the loaded epoch at or below [after], read again from just
     * before it (at most [REWIND_MAX_PAGES] pages, until a page reaches past
     * [after] or the journal ends). Returns where the rows start and the rows.
     * The epoch is read under the provider lock, briefly; nothing else here
     * touches MLS state.
     */
    private suspend fun rewoundIfRolledBack(
        groupIdHex: String,
        record: GroupRecord,
        after: Long,
        first: me.lakko.app.ds.FetchResult,
    ): Pair<Long, me.lakko.app.ds.FetchResult> {
        val commits = first.registeredCommits ?: return after to first
        val recorded = try {
            GroupSettingsStore.cursorEpoch(applicationContext, currentAccountIdB64.get(), groupIdHex)
        } catch (_: Throwable) {
            null
        } ?: return after to first
        val loaded = providerLock.withLock {
            runCatching { record.group.epoch() }.getOrNull()
        }
        val from = snapshotRewindFrom(after, loaded, recorded, commits) ?: return after to first
        android.util.Log.w(FAILED_ROW_LOG_TAG, rewindLine(loaded ?: 0uL, recorded))
        var page = chatClient.fetchGroupJournal(groupId = record.groupId, after = from, waitSecs = 0)
        val rows = page.messages.toMutableList()
        var pages = 1
        while (pages < REWIND_MAX_PAGES && page.hasMore) {
            val last = rows.lastOrNull()?.id ?: from
            if (last >= after) break
            page = chatClient.fetchGroupJournal(groupId = record.groupId, after = last, waitSecs = 0)
            rows.addAll(page.messages)
            pages += 1
        }
        return from to page.copy(messages = rows)
    }

    /** ADR-0091 phase 1b — record the group's epoch with its cursor. Runs in
     *  the poll's locked apply region. */
    private fun saveCursorEpochBestEffort(groupIdHex: String, record: GroupRecord) {
        val acct = currentAccountIdB64.get() ?: return
        val epoch = runCatching { record.group.epoch() }.getOrNull() ?: return
        try {
            GroupSettingsStore.saveCursorEpoch(applicationContext, acct, groupIdHex, epoch)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "cursor epoch not saved: ${cause.javaClass.simpleName}")
        }
    }

    /** Section 5 — save the knock position once everything read up to it is
     *  saved (the MLS snapshot with the retained requests is durable). */
    private fun saveKnockCursorBestEffort(groupIdHex: String, cursor: Long) {
        val acct = currentAccountIdB64.get() ?: return
        try {
            GroupSettingsStore.saveKnockCursor(applicationContext, acct, groupIdHex, cursor)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087 knock position not saved: ${cause.javaClass.simpleName}")
        }
    }

    /** The profile a knock carries: name and photo, no grants. Empty when
     *  none is stored. */
    private fun ownKnockProfile(): ByteArray {
        val accountIdB64 = currentAccountIdB64.get() ?: return ByteArray(0)
        val accountId = try {
            java.util.Base64.getDecoder().decode(accountIdB64)
        } catch (_: IllegalArgumentException) {
            return ByteArray(0)
        }
        return try {
            me.lakko.app.profile.OwnProfileStore.load(applicationContext, accountId)
                ?.toFfi(accountId = accountId, shareUsername = false)
                ?.let { uniffi.lakko_ffi_uniffi.composeProfileEnvelope(it) }
                ?: ByteArray(0)
        } catch (_: Throwable) {
            ByteArray(0)
        }
    }

    // -----------------------------------------------------------------
    // ADR-0087 sections 2-5, 8 and 11 — the group's @name and type, and a
    // request to join from search
    // -----------------------------------------------------------------

    /**
     * group-and-channel-privacy.md 4.5.4 — the public record carries the
     * group's name and photo, so a change to either, whoever made it, owes a
     * republish of the record. Recorded durably here and sent by
     * [maintainGroupHandleIfDue] at the tail of the group's polls, until it
     * lands; only the owner's device (it holds the handle key) acts on it.
     * A type change already owed carries the same republish and is kept.
     */
    private fun owePublicRecordRepublish(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            val handle = GroupSettingsStore.handle(applicationContext, accountIdB64, groupIdHex)
                ?.takeIf { it.public } ?: return
            if (GroupSettingsStore.owedTypeChange(applicationContext, accountIdB64, groupIdHex) == null) {
                GroupSettingsStore.saveOwedTypeChange(applicationContext, accountIdB64, groupIdHex, true to handle.handle)
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087 4.5.4 record republish not recorded", cause)
        }
    }

    /** What a public record of [groupIdHex] says, or `null` when the group
     *  has no name (a public group needs one) or no knock key. */
    private suspend fun publicFactsFor(groupIdHex: String): PublicGroupFacts? {
        val record = groups[groupIdHex] ?: return null
        val name = groupName(groupIdHex)?.name?.takeIf { it.isNotBlank() } ?: return null
        // Section 5 — the record names the group's current knock key and its
        // version; a searcher seals to that version.
        val knock = providerLock.sessionLock { groupKnockKeyFor(record, groupIdHex) } ?: return null
        return PublicGroupFacts(
            name = name,
            description = "",
            memberCount = memberCount(groupIdHex) ?: 1,
            knockPublicKey = knock.first,
            avatar = groupPhoto(groupIdHex)?.avatar,
            knockVersion = knock.second,
        )
    }

    /**
     * Section 11 — `Delete group`, for an owner alone: retire the @name (a
     * network failure throws and changes nothing), then end the conversation
     * as section 9 does, so the `conversation_ended` register reaches every
     * device of the account and each wipes it (`deleteAfterEndedElsewhere`).
     */
    public suspend fun deleteSoloGroup(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: throw IllegalStateException("no session")
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        val account = rootAccountIdentity() ?: throw IllegalStateException("no account")
        groupHandle(groupIdHex)?.let { h ->
            retireGroupHandle(discovery, account.ed25519Seed(), record.groupId, h.handle)
        }
        val epoch = providerLock.sessionLock { runCatching { record.group.epoch() }.getOrDefault(0uL) }
        val hex = groupIdHex.lowercase()
        if (!membershipEnds.save(accountIdB64, appliedEnd(hex, groupName(hex)?.name.orEmpty(), true, epoch))) {
            throw IllegalStateException("end not recorded")
        }
        endingGroups += hex
        publishThreadList()
        pushScope.launchSession { runOwedMembershipEnds() }
    }

    /** Section 3.3 — the live check while typing a public @name: `true` when
     *  the name answers a lookup, `null` when it could not be asked. */
    public suspend fun handleVisiblyTaken(handle: String): Boolean? = try {
        discovery.nameAnswers(canonicalHandleInput(handle))
    } catch (cause: kotlinx.coroutines.CancellationException) {
        throw cause
    } catch (_: Throwable) {
        null
    }

    /** Section 5 — the public groups and channels matching [query]. */
    public suspend fun searchPublicConversations(query: String): List<FoundConversation> =
        me.lakko.app.chat.searchPublicConversations(discovery, query)

    /** Section 10 — whether this device created the group. */
    public fun createdGroup(groupIdHex: String): Boolean =
        GroupSettingsStore.created(applicationContext, currentAccountIdB64.get(), groupIdHex)

    /** Whether this device holds the group [groupIdHex] (search's `Open`).
     *  A held group also clears its `Requests sent` row (joined, 6.2). */
    public fun holdsGroup(groupIdHex: String): Boolean {
        val held = groups.containsKey(groupIdHex.lowercase())
        if (held) RequestsSentStore.drop(applicationContext, groupIdHex)
        return held
    }

    /** Section 6.2 — the requests this device sent, opened for the bound
     *  account; joined ones leave. */
    public fun requestsSent(): StateFlow<List<SentRequest>> {
        currentAccountIdB64.get()?.let { RequestsSentStore.open(applicationContext, it) }
        for (r in RequestsSentStore.rows.value) {
            if (groups.containsKey(r.idHex)) RequestsSentStore.drop(applicationContext, r.idHex)
        }
        return RequestsSentStore.rows
    }

    /** Section 3.1 — the @name claim a New group form runs before the group
     *  leaves the device ([startGroupAlone]'s hook). */
    public suspend fun claimHandleForNewGroup(
        groupId: ByteArray,
        handle: String,
        public: Boolean,
        facts: PublicGroupFacts?,
    ): SignedGroupHandle {
        val account = rootAccountIdentity() ?: throw IllegalStateException("no account")
        // ADR-0087 section 3 (note of 2026-11-14): this attempt's group id is
        // kept before any request about the name, so a retry after a failed
        // step or a lost answer moves the name from an earlier attempt.
        val accountIdB64 = currentAccountIdB64.get() ?: throw IllegalStateException("no bound account")
        val earlier = GroupHandleClaims.on(applicationContext)
            .keep(accountIdB64, handle, DsClient.pseudonymHex(groupId))
            .mapNotNull { hexToBytes(it) }
        val seed = account.ed25519Seed()
        return claimOrTakeBackGroupHandle(
            earlier,
            claim = { claimGroupHandle(discovery, seed, groupId, handle, public, facts) },
            rekeyFrom = { earlierId -> moveGroupHandle(discovery, seed, earlierId, groupId, handle) },
            update = { updateGroupHandle(discovery, seed, groupId, handle, public, facts) },
        )
    }

    /** ADR-0087 section 3 — the New group of [handle] exists, or the name is
     *  someone else's: the earlier attempts are no longer needed. */
    public fun forgetNewGroupHandle(handle: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        GroupHandleClaims.on(applicationContext).forget(accountIdB64, handle)
    }

    /** Whether a New group on this account claimed [handle] and has not
     *  finished: the name is this account's to take back, not "taken". */
    public fun isNewGroupHandleKept(handle: String): Boolean {
        val accountIdB64 = currentAccountIdB64.get() ?: return false
        return GroupHandleClaims.on(applicationContext).isClaimed(accountIdB64, handle)
    }

    /** Store a handle this owner just wrote and send it into the group. */
    public suspend fun adoptSignedHandle(groupIdHex: String, signed: SignedGroupHandle) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        // The owner's own write: the certificate came from discovery in the
        // answer to this device's own signed statement.
        GroupSettingsStore.saveHandle(applicationContext, accountIdB64, groupIdHex, signed.stored.copy(verified = true))
        // Section 3 — the lease the write answered.
        val lease = signed.leaseUntilMs ?: (System.currentTimeMillis() + HANDLE_LEASE_MS)
        GroupSettingsStore.saveLeaseUntil(applicationContext, accountIdB64, groupIdHex, lease)
        publishThreadList()
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        // Walk 3, W3-R5 — stored first by design: while this device is
        // behind it is not sent now; the back-in-step re-send carries it.
        if (behindStopsActing(groupIdHex)) return
        try {
            forSession { broadcastControlToGroup(identity, groupIdHex, record.group, signed.envelope, automatic = false) }
        } catch (cause: Throwable) {
            // The growth re-send relays it later; the name is held either way.
            android.util.Log.w("ChatRepository", "ADR-0087 handle envelope not sent", cause)
        }
    }

    /**
     * Section 3.1 / 4.4 — take [handle] for a group this account owns, public
     * or private, and tell the members. Throws [me.lakko.app.discovery.HandleWriteError]
     * (`name-taken`, ...) or a network error; the caller maps it to copy.
     */
    public suspend fun claimHandleForGroup(groupIdHex: String, handle: String, public: Boolean) {
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        val account = rootAccountIdentity() ?: throw IllegalStateException("no account")
        val facts = if (public) publicFactsFor(groupIdHex) ?: throw IllegalStateException("public group needs a name") else null
        val signed = claimGroupHandle(discovery, account.ed25519Seed(), record.groupId, handle, public, facts)
        adoptSignedHandle(groupIdHex, signed)
    }

    /**
     * Section 4.1 / 8 — the owner's Group type screen: flip public / private
     * and, when [newHandle] differs, move to it. Going private re-sends the
     * photo under a fresh key (18.3).
     */
    public suspend fun changeGroupType(groupIdHex: String, public: Boolean, newHandle: String) {
        // Section 8 — recorded as owed before the first step, retried until it
        // lands ([maintainGroupHandleIfDue]).
        currentAccountIdB64.get()?.let {
            GroupSettingsStore.saveOwedTypeChange(applicationContext, it, groupIdHex, public to canonicalHandleInput(newHandle))
        }
        changeGroupTypeNow(groupIdHex, public, newHandle)
    }

    private suspend fun changeGroupTypeNow(groupIdHex: String, public: Boolean, newHandle: String) {
        val record = groups[groupIdHex] ?: throw IllegalStateException("unknown group")
        val account = rootAccountIdentity() ?: throw IllegalStateException("no account")
        val current = groupHandle(groupIdHex)
        val facts = if (public) publicFactsFor(groupIdHex) ?: throw IllegalStateException("public group needs a name") else null
        val wanted = canonicalHandleInput(newHandle)
        val signed = when {
            current == null -> claimGroupHandle(discovery, account.ed25519Seed(), record.groupId, wanted, public, facts)
            current.handle != wanted ->
                replaceGroupHandle(discovery, account.ed25519Seed(), record.groupId, current.handle, wanted, public, facts)
            else -> updateGroupHandle(discovery, account.ed25519Seed(), record.groupId, wanted, public, facts)
        }
        adoptSignedHandle(groupIdHex, signed)
        currentAccountIdB64.get()?.let { GroupSettingsStore.saveOwedTypeChange(applicationContext, it, groupIdHex, null) }
        if (current?.public == true && !public) reuploadGroupPhotoBestEffort(groupIdHex)
    }

    /** Lease renewals this session (section 3: at most 20 per foreground
     *  session). */
    private val leaseRenewalsThisSession = java.util.concurrent.atomic.AtomicInteger(0)

    /**
     * Section 3 / 8 — at the tail of a poll on an owner device: retry an owed
     * public / private change, and renew a handle whose lease has fewer than
     * 90 days left (at most 20 renewals a session). Best effort.
     */
    private suspend fun maintainGroupHandleIfDue(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val record = groups[groupIdHex] ?: return
        val owner = providerLock.sessionLock { ownsGroupAsOwner(record.group) }
        if (!owner) return
        // Section 3 — a rekey owed after the owner's rotation goes first: every
        // other handle write is signed with the new key.
        GroupSettingsStore.owedRekeySeed(applicationContext, accountIdB64, groupIdHex)?.let { oldSeed ->
            try {
                val stored = GroupSettingsStore.handle(applicationContext, accountIdB64, groupIdHex)
                if (stored == null) {
                    GroupSettingsStore.saveOwedRekeySeed(applicationContext, accountIdB64, groupIdHex, null)
                } else {
                    val account = rootAccountIdentity() ?: return
                    val signed = rekeyGroupHandle(
                        discovery,
                        oldSeed,
                        account.ed25519Seed(),
                        record.groupId,
                        stored.handle,
                        stored.public,
                    )
                    // Re-sends the in-group envelope under the new key.
                    adoptSignedHandle(groupIdHex, signed)
                    GroupSettingsStore.saveOwedRekeySeed(applicationContext, accountIdB64, groupIdHex, null)
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                android.util.Log.i("ChatRepository", "ADR-0087 owed handle rekey not applied yet")
                return
            } finally {
                oldSeed.fill(0)
            }
        }
        GroupSettingsStore.owedTypeChange(applicationContext, accountIdB64, groupIdHex)?.let { (public, handle) ->
            try {
                changeGroupTypeNow(groupIdHex, public, handle)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.i("ChatRepository", "ADR-0087 owed type change not applied yet")
            }
            return
        }
        val handle = groupHandle(groupIdHex) ?: return
        val until = GroupSettingsStore.leaseUntil(applicationContext, accountIdB64, groupIdHex)
        val now = System.currentTimeMillis()
        if (until != null && until - now > HANDLE_RENEW_BEFORE_MS) return
        if (leaseRenewalsThisSession.incrementAndGet() > HANDLE_RENEWALS_PER_SESSION) return
        try {
            changeGroupTypeNow(groupIdHex, handle.public, handle.handle)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.i("ChatRepository", "ADR-0087 handle lease not renewed yet")
        }
    }

    /** 18.3 — a group that went private sends its photo again under a fresh
     *  key, so the keys the public record carried open nothing new. */
    private suspend fun reuploadGroupPhotoBestEffort(groupIdHex: String) {
        val photo = groupPhoto(groupIdHex)?.avatar ?: return
        try {
            val stillBytes = me.lakko.app.avatar.AvatarImages.cache(applicationContext).get(photo.still.blobIdHex) ?: return
            val still = AttachmentBridge.encryptAndUpload(bytes = stillBytes, mime = "image/jpeg", name = null)
            val stillBlob = me.lakko.app.profile.avatarBlobOf(still.blobId, still.key, still.nonce, stillBytes.size.toLong(), photo.still.side)
            me.lakko.app.avatar.AvatarImages.putOwn(applicationContext, stillBlob, stillBytes)
            setGroupPhoto(groupIdHex, me.lakko.app.profile.AvatarRefs(still = stillBlob))
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087 18.3 photo not re-sent", cause)
        }
    }

    /** Section 11 — retire the group's @name (Delete group), best effort. */
    public suspend fun retireGroupHandleBestEffort(groupIdHex: String) {
        val record = groups[groupIdHex] ?: return
        val handle = groupHandle(groupIdHex) ?: return
        val account = rootAccountIdentity() ?: return
        try {
            retireGroupHandle(discovery, account.ed25519Seed(), record.groupId, handle.handle)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087 handle not retired", cause)
        }
    }

    /**
     * Section 5 / UX 5.1 — ask to join a public group found in search: a
     * fresh KeyPackage sealed to the knock key its record names, with a
     * reply grant and the own profile, no capability. One `ds-knock` token.
     */
    public suspend fun requestToJoinPublicGroup(found: FoundConversation) {
        sendKnockFromSearch(found.id, found.knockOrReadKey, found.knockVersion, found.kind)
        val hex = found.id.joinToString("") { "%02x".format(it) }
        currentAccountIdB64.get()?.let { RequestsSentStore.open(applicationContext, it) }
        RequestsSentStore.record(
            applicationContext,
            SentRequest(
                idHex = hex,
                name = found.name,
                handle = found.handle,
                sentAtMs = System.currentTimeMillis(),
                avatar = found.avatar,
                knockKeyB64 = java.util.Base64.getEncoder().encodeToString(found.knockOrReadKey),
                knockVersion = found.knockVersion.toInt(),
            ),
        )
    }

    /**
     * Section 6.2 — the group-invite page's request (`Request to join`):
     * [requestToJoinFromLink], then the request is recorded under `Requests
     * sent` with what `Ask again` needs (the capability and the ledger id, no
     * link secret), so the list shows it and the page opened again within the
     * day shows the wait instead of a second request.
     */
    public suspend fun requestToJoinFromLinkRecorded(
        capabilityWire: ByteArray,
        nowMs: ULong,
        blobId: ByteArray,
        name: String?,
    ): Pair<ByteArray, ByteArray> {
        val result = requestToJoinFromLink(capabilityWire, nowMs, blobId)
        currentAccountIdB64.get()?.let { RequestsSentStore.open(applicationContext, it) }
        val encoder = java.util.Base64.getEncoder()
        RequestsSentStore.record(
            applicationContext,
            SentRequest(
                idHex = DsClient.pseudonymHex(result.first).lowercase(),
                name = name.orEmpty(),
                handle = "",
                sentAtMs = System.currentTimeMillis(),
                capabilityB64 = encoder.encodeToString(capabilityWire),
                blobIdB64 = encoder.encodeToString(blobId),
            ),
        )
        return result
    }

    /** Section 6.2 — the request this device sent to [groupIdHex] that has
     *  not led to the group yet, or `null`. A held group's row is dropped. */
    public fun openSentRequest(groupIdHex: String): SentRequest? {
        currentAccountIdB64.get()?.let { RequestsSentStore.open(applicationContext, it) }
        if (holdsGroup(groupIdHex)) return null
        return RequestsSentStore.of(groupIdHex)
    }

    /** Section 6.2 — `Ask again` on a request a day old or more, from the
     *  `Requests sent` list and from the invite page alike: the same request
     *  again, and the day starts over. A link request goes through its link
     *  again ([requestToJoinFromLinkRecorded]); a request from search is sealed
     *  again to the knock key the public record named. */
    public suspend fun askAgain(request: SentRequest) {
        if (request.fromLink) {
            val decoder = java.util.Base64.getDecoder()
            requestToJoinFromLinkRecorded(
                decoder.decode(request.capabilityB64),
                System.currentTimeMillis().toULong(),
                decoder.decode(request.blobIdB64),
                request.name,
            )
            return
        }
        val id = hexToBytes(request.idHex) ?: throw IllegalArgumentException("bad id")
        val key = java.util.Base64.getDecoder().decode(request.knockKeyB64)
        sendKnockFromSearch(id, key, request.knockVersion.toUShort(), uniffi.lakko_ffi_uniffi.FfiHandleRecordKind.GROUP)
        RequestsSentStore.record(applicationContext, request.copy(sentAtMs = System.currentTimeMillis()))
    }

    private suspend fun sendKnockFromSearch(
        groupId: ByteArray,
        knockKey: ByteArray,
        knockVersion: UShort,
        kind: uniffi.lakko_ffi_uniffi.FfiHandleRecordKind,
    ) {
        val found = object {
            val id = groupId
            val knockOrReadKey = knockKey
            val kind = kind
        }
        val identity = canonicalIdentity.get() ?: throw IllegalStateException("not bootstrapped")
        if (found.kind != uniffi.lakko_ffi_uniffi.FfiHandleRecordKind.GROUP || found.id.size != 32) {
            throw IllegalArgumentException("not a group")
        }
        ensureSession()
        val grant = me.lakko.app.reach.ReachManager.get(applicationContext).mintReply(toKey = found.id)
        val row = forSession {
            providerLock.sessionLock {
                val keyPackage = identity.createKeyPackage()
                persistSnapshotDurableOrThrow()
                uniffi.lakko_ffi_uniffi.sealKnockRow(
                    found.knockOrReadKey,
                    knockVersion,
                    keyPackage,
                    grant,
                    ownKnockProfile(),
                    null,
                )
            }
        }
        dsClient.knock(found.id, listOf(row))
    }

    // -----------------------------------------------------------------
    // ADR-0073 "Phase 2 revised: silent continuity" - a member's account-key
    // reset was ACCEPTED by the identity gate. Nothing here has a surface: no
    // copy, no row, no flag (R.0).
    // -----------------------------------------------------------------

    /** The accepted generation and the retired keys per peer account, read
     *  directly (the identity gate writes them; this side only reads). */
    private val resetLedgerStore: me.lakko.app.contacts.ResetLedgerStore =
        me.lakko.app.contacts.SharedPrefsResetLedgerStore(applicationContext)

    /** What this device verified for accounts it never pinned (read only). */
    private val verifiedBindingStore: me.lakko.app.contacts.VerifiedBindingStore =
        me.lakko.app.contacts.SharedPrefsVerifiedBindingStore(applicationContext)

    /** R.6 - the claim state of every old group (accepted / pending / own). */
    internal val successorClaims: SuccessorClaimBackend = SealedSuccessorClaimBackend.onDevice(applicationContext)

    /** R.5 / R.7 - this device's own facts about moved conversations. */
    private val continuityStore = ContinuityStore(SharedPrefsContinuityBackend(applicationContext))

    /**
     * R.2 "Triggers" - the identity check the scheduled device-list arm runs
     * for every roster account whose account id this device knows, through
     * the contacts repository's fail-closed gate (which is what accepts an
     * attested reset). The second argument is the roster's person key for an
     * account this device never pinned. Set by the contacts repository at
     * construction; `null` runs no check (tests).
     */
    @Volatile
    public var contactIdentityCheck:
        (suspend (accountId: ByteArray, heldKeyB64: String?) -> me.lakko.app.contacts.KnownRequestCheck)? = null

    /** The superseded keys of the signed-in account, cached briefly: the
     *  ledger is a preferences row and every send asks. */
    @Volatile
    private var supersededCache: Triple<String, Long, Map<String, String>>? = null

    /** R.5 / R.6 - every key an accepted reset retired -> its account id
     *  (standard base64 on both sides). */
    internal fun supersededKeys(): Map<String, String> {
        val account = currentAccountIdB64.get() ?: return emptyMap()
        val now = android.os.SystemClock.elapsedRealtime()
        supersededCache?.let { (owner, at, keys) ->
            if (owner == account && now - at < SUPERSEDED_CACHE_MS) return keys
        }
        val keys = try {
            me.lakko.app.contacts.supersededKeysOf(resetLedgerStore.loadAll(account))
        } catch (_: Throwable) {
            emptyMap()
        }
        supersededCache = Triple(account, now, keys)
        return keys
    }

    /** The unrepairable key of each group as last computed (empty = none). */
    private val unrepairableCache = java.util.concurrent.ConcurrentHashMap<String, java.util.Optional<String>>()

    private fun invalidateDeadSession(groupIdHex: String) {
        unrepairableCache.remove(groupIdHex.lowercase())
    }

    /** What [continueConversation] and the guards read off one group. */
    private fun continuityFactsOf(groupIdHex: String): ContinuityFacts? {
        val group = groups[groupIdHex]?.group ?: return null
        val roster = try {
            rosterOf(group).map { outboxB64(personKeyOf(it)) }
        } catch (_: Throwable) {
            return null
        }
        val table = roleTableOrNull(group)
        return ContinuityFacts(
            kind = conversationKindOf(groupKind(groupIdHex)),
            rosterPersonKeysB64 = roster.distinct(),
            ownerKeyB64 = table?.owner?.let { outboxB64(it) },
            adminKeysB64 = table?.admins?.map { outboxB64(it) }.orEmpty(),
            active = readIsActive { group.isActive() } != false,
        )
    }

    /**
     * R.6 (i) - the key whose accepted reset makes [groupIdHex] unrepairable
     * ([unrepairableBy]): the other person of a one-to-one, or the owner of a
     * group. `null` when it is repairable, nothing was accepted, or the
     * roster cannot be read.
     */
    internal fun unrepairableKeyOf(groupIdHex: String): String? {
        val superseded = supersededKeys()
        if (superseded.isEmpty()) return null
        val key = groupIdHex.lowercase()
        return unrepairableCache.getOrPut(key) {
            val facts = continuityFactsOf(groupIdHex)
            java.util.Optional.ofNullable(
                facts?.let {
                    unrepairableBy(
                        kind = it.kind,
                        rosterPersonKeysB64 = it.rosterPersonKeysB64,
                        ownPersonKeyB64 = ownPersonKeyB64(),
                        ownerKeyB64 = it.ownerKeyB64,
                        supersededKeys = superseded.keys,
                    )
                },
            )
        }.orElse(null)
    }

    /**
     * R.7 case 1 - is this group a DEAD SESSION ([isDeadSession]): a
     * one-to-one whose other person's reset this device accepted? Into such a
     * group nothing is published but the successor claim. Every send entry
     * point asks this first.
     */
    internal fun deadOneToOne(groupIdHex: String): Boolean =
        isDeadSession(conversationKindOf(groupKind(groupIdHex)), unrepairableKeyOf(groupIdHex))

    /**
     * R.7 case 1 — apply an edit, a delete or a reaction of this account on
     * this device only ([OpRoute.LOCAL_ONLY]). Nothing is published and no
     * ratchet moves. The founder's re-send of its own messages reads the
     * stored text, which is then the edited text, and skips a deleted one.
     */
    private suspend fun applyOwnOpLocally(
        groupIdHex: String,
        op: MessageOp,
        targetMessageId: Long,
        text: String,
        emoji: String?,
        reactionAdd: Boolean = true,
    ) {
        val record = groups[groupIdHex] ?: return
        groupLock(groupIdHex).withLock {
            val ownKey = providerLock.withLock { ownIdentity(record.group) }
            record.applyOwnOp(
                op = op,
                target = targetMessageId,
                text = text,
                emoji = emoji,
                ownKey = ownKey,
                reactionAdd = reactionAdd,
            )
            persistMessageLogBestEffort()
            publishThreadList()
        }
    }

    // ---- R.7 case 1: the held send ------------------------------------

    /** One held message: the pending row it shows as, and when it was held. */
    private class HeldMessage(
        val groupIdHex: String,
        val localId: Long,
        val heldAtMs: Long,
        val text: String,
        val attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef>,
        val reply: ReplyRef?,
        val forward: ForwardRef?,
        /** ADR-0092 — the content the message carries, kept while it waits. */
        val content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    )

    /**
     * The held messages, by conversation id (hex of the lineage root), in
     * send order. Mirrored to [heldSendStore] on every change, so a held
     * message survives a process death ([restoreHeldSends]).
     */
    private val heldSends = java.util.concurrent.ConcurrentHashMap<String, MutableList<HeldMessage>>()

    /** The durable copy of [heldSends] (sealed under the Keystore key). */
    private val heldSendStore: HeldSendBackend = SealedHeldSendBackend(applicationContext)

    /**
     * ADR-0079 section 5 (reworked 2026-10-17) - what was written to a person
     * added through their invite link before their app answered, in the ONE
     * held-send queue: keyed by the pending contact's queue key (the
     * inviter's account id, 32 hex characters, never a conversation id), in
     * the order written. Stored with [heldSends] in [heldSendStore]; never
     * flushed from here: [handOverPendingInvitedMessages] moves it into the
     * queue of the one-to-one once that exists. Mirror of the web
     * `pendingQueueKey` slot and the iOS `ChatRepository+PendingInvited`.
     */
    private val pendingInvitedHeld = java.util.concurrent.ConcurrentHashMap<String, MutableList<PersistedHeldSend>>()

    /** Keep one message written to a pending contact. `false` when it is
     *  empty (the caller keeps the words). */
    public fun queuePendingInvitedMessage(queueKey: String, text: String): Boolean {
        if (text.isEmpty() || !PendingInvitedQueue.isQueueKey(queueKey)) return false
        val row = PersistedHeldSend(
            conversationHex = queueKey,
            groupIdHex = queueKey,
            heldAtMs = System.currentTimeMillis(),
            text = text,
        )
        pendingInvitedHeld.getOrPut(queueKey) { java.util.Collections.synchronizedList(ArrayList()) }.add(row)
        persistHeldSends()
        return true
    }

    /** What was written to the pending contact [queueKey], in order: what
     *  this device holds to send, and the display-only copies of what was
     *  written on another of this account's devices ([HeldCopyStore], state
     *  bundle v3), which this device never sends. */
    public fun pendingInvitedMessages(queueKey: String): List<PersistedHeldSend> {
        val own = pendingInvitedHeld[queueKey]?.let { synchronized(it) { it.toList() } }.orEmpty()
        val account = currentAccountIdB64.get() ?: return own
        val copies = try {
            heldCopyStore.copies(account, queueKey)
        } catch (_: Throwable) {
            emptyList()
        }
        if (copies.isEmpty()) return own
        val shown = copies.map {
            PersistedHeldSend(conversationHex = queueKey, groupIdHex = queueKey, heldAtMs = it.heldAtMs, text = it.text)
        }
        return (own + shown).sortedBy { it.heldAtMs }
    }

    /** B deletes a pending contact, or the account is wiped: what was
     *  written to them goes, and so do the display copies. */
    public fun clearPendingInvitedMessages(queueKey: String) {
        if (pendingInvitedHeld.remove(queueKey) != null) persistHeldSends()
        val account = currentAccountIdB64.get() ?: return
        try {
            heldCopyStore.clear(account, queueKey)
        } catch (_: Throwable) {
            // best-effort
        }
    }

    // ---- display copies of messages held on another device (bundle v3) ----

    /** The copy store (sealed, per account). */
    private val heldCopyStore: HeldCopyStore = HeldCopyStore.onDevice(applicationContext)

    /** The rows the display copies show in a one-to-one, by group id hex:
     *  each copy with the local id of its waiting row. In memory; rebuilt
     *  from [heldCopyStore] at every bootstrap ([restoreHeldCopies]). */
    private val heldCopyRows = java.util.concurrent.ConcurrentHashMap<String, MutableList<Pair<HeldCopy, Long>>>()

    /** Show [copies] in [record] as own outgoing rows in the waiting state.
     *  Never enqueued: this device does not send them. */
    private fun showHeldCopies(groupIdHex: String, record: GroupRecord, copies: List<HeldCopy>) {
        if (copies.isEmpty()) return
        val rows = heldCopyRows.getOrPut(groupIdHex) { java.util.Collections.synchronizedList(ArrayList()) }
        for (copy in copies) {
            val localId = record.nextLocalId()
            record.appendPending(
                localId = localId,
                text = copy.text,
                attachments = emptyList(),
                createdAt = java.time.Instant.ofEpochMilli(copy.heldAtMs).toString(),
                authorKey = null,
            )
            rows += copy to localId
        }
    }

    /** At bootstrap: the copies already moved to a one-to-one show again. */
    private fun restoreHeldCopies() {
        val account = currentAccountIdB64.get() ?: return
        if (heldCopyRows.isNotEmpty()) return
        val all = try {
            heldCopyStore.load(account)
        } catch (_: Throwable) {
            return
        }
        var shown = false
        for ((key, copies) in all) {
            if (PendingInvitedQueue.isQueueKey(key)) continue
            val record = groups[key] ?: continue
            showHeldCopies(key, record, copies)
            shown = true
        }
        if (shown) publishThreadList()
    }

    /**
     * A message from this account's OTHER device landed in [groupIdHex]: it
     * replaces the first display copy with the same text and the same number
     * of attachments (the real message is what shows now). Called after the
     * batch was consumed; this device's own sends never arrive here (they are
     * own-echo filtered).
     */
    private fun settleHeldCopies(groupIdHex: String, record: GroupRecord, events: List<IncomingChatEvent>) {
        val rows = heldCopyRows[groupIdHex] ?: return
        val account = currentAccountIdB64.get() ?: return
        val own = ownIdentity(record.group) ?: return
        for (event in events) {
            if (event !is IncomingChatEvent.Application || event.senderKey != own) continue
            val removed = try {
                heldCopyStore.replace(account, groupIdHex, event.text, event.attachments.size)
            } catch (_: Throwable) {
                null
            } ?: continue
            synchronized(rows) {
                val at = rows.indexOfFirst { it.first == removed }
                if (at >= 0) record.removeLocal(rows.removeAt(at).second)
            }
        }
        if (rows.isEmpty()) heldCopyRows.remove(groupIdHex)
    }

    /**
     * Move what was written to the pending contact [queueKey] into the
     * held-send queue of the one-to-one [toGroupIdHex] just joined, in order,
     * each shown as a sending row there, then flush it. The give-up clock
     * starts now (they could not be sent before the conversation existed).
     * The flush sends them in order; a send that fails keeps that message and
     * the rest for the next attempt (the next poll of the conversation).
     */
    public fun handOverPendingInvitedMessages(queueKey: String, toGroupIdHex: String): Boolean {
        val record = groups[toGroupIdHex] ?: return false
        val rows = pendingInvitedHeld.remove(queueKey)?.let { synchronized(it) { it.toList() } }.orEmpty()
        val now = System.currentTimeMillis()
        for (row in rows) {
            val localId = record.nextLocalId()
            record.appendPending(
                localId = localId,
                text = row.text,
                attachments = emptyList(),
                createdAt = java.time.Instant.ofEpochMilli(now).toString(),
                authorKey = null,
                reply = null,
                forward = null,
            )
            enqueueHeld(HeldMessage(toGroupIdHex, localId, now, row.text, emptyList(), null, null), persist = false)
        }
        persistHeldSends()
        // State bundle v3 — what was written on another device moves too, as
        // display-only waiting rows (never sent from here).
        val copies = currentAccountIdB64.get()?.let { account ->
            try {
                heldCopyStore.move(account, queueKey, toGroupIdHex)
            } catch (_: Throwable) {
                emptyList()
            }
        }.orEmpty()
        showHeldCopies(toGroupIdHex, record, copies)
        if (copies.isNotEmpty() && rows.isEmpty()) publishThreadList()
        if (rows.isNotEmpty()) {
            publishThreadList()
            val conversation = lineageNow().conversationOf(toGroupIdHex.lowercase())
            pushScope.launch { withContext(mlsDispatcher) { settleHeld(conversation) } }
        }
        return true
    }

    /** Write the held queue as it stands. Best-effort: a queue that cannot
     *  be written is still sent in this process. */
    private fun persistHeldSends() {
        val account = currentAccountIdB64.get() ?: return
        val rows = ArrayList<PersistedHeldSend>()
        for ((conversationHex, queue) in heldSends) {
            val items = synchronized(queue) { queue.toList() }
            for (h in items) {
                rows += PersistedHeldSend(
                    conversationHex = conversationHex,
                    groupIdHex = h.groupIdHex,
                    heldAtMs = h.heldAtMs,
                    text = h.text,
                    attachments = h.attachments.map(PersistedAttachment.Companion::from),
                    reply = h.reply?.let { PersistedReply(target = it.target, author = it.author, excerpt = it.excerpt) },
                    forward = h.forward?.let {
                        PersistedForward(
                            author = it.author,
                            originGroupIdHex = it.originGroupIdHex,
                            originMessageId = it.originMessageId,
                        )
                    },
                    contentEnvelopeB64 = h.content?.let { c ->
                        composeContentBytes(c, h.attachments)?.let { java.util.Base64.getEncoder().encodeToString(it) }
                    },
                )
            }
        }
        // ADR-0079 - the messages to pending contacts ride the same store.
        for ((_, queue) in pendingInvitedHeld) rows += synchronized(queue) { queue.toList() }
        rows.sortBy { it.heldAtMs }
        try {
            if (!heldSendStore.save(account, rows)) {
                android.util.Log.w("ChatRepository", "ADR-0073 held messages not stored")
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0073 held messages not stored", cause)
        }
    }

    /**
     * Read the held messages back after a process death: each shows again as
     * a "Sending..." row in the group it was written in, and waits for the
     * conversation's successor from now ([restoredHeldSends]). Runs once per
     * bootstrap, after the groups and the message log are in memory.
     */
    private fun restoreHeldSends() {
        val account = currentAccountIdB64.get() ?: return
        if (heldSends.isNotEmpty()) return
        val stored = try {
            restoredHeldSends(heldSendStore.load(account), System.currentTimeMillis())
        } catch (_: Throwable) {
            return
        }
        if (stored.isEmpty()) return
        for (row in stored) {
            // ADR-0079 - a message to a pending contact waits under its
            // queue key for the hand-over; it is never flushed from there.
            if (PendingInvitedQueue.isQueueKey(row.conversationHex) && groups[row.groupIdHex] == null) {
                pendingInvitedHeld.getOrPut(row.conversationHex) {
                    java.util.Collections.synchronizedList(ArrayList())
                }.add(row)
                continue
            }
            val attachments = row.attachments.mapNotNull { it.toAttachmentRef() }
            val content = row.contentEnvelopeB64
                ?.let { runCatching { java.util.Base64.getDecoder().decode(it) }.getOrNull() }
                ?.let(::parseContentBytes)
            if (row.text.isEmpty() && attachments.isEmpty() && content == null) continue
            val reply = row.reply?.let { ReplyRef(target = it.target, author = it.author, excerpt = it.excerpt) }
            val forward = row.forward?.let {
                ForwardRef(author = it.author, originGroupIdHex = it.originGroupIdHex, originMessageId = it.originMessageId)
            }
            val record = groups[row.groupIdHex]
            val localId = record?.nextLocalId() ?: 0L
            record?.appendPending(
                localId = localId,
                text = row.text,
                attachments = attachments,
                createdAt = java.time.Instant.ofEpochMilli(row.heldAtMs).toString(),
                authorKey = null,
                reply = reply,
                forward = forward,
                content = content,
            )
            enqueueHeld(
                HeldMessage(row.groupIdHex, localId, row.heldAtMs, row.text, attachments, reply, forward, content),
                persist = false,
            )
        }
        publishThreadList()
    }

    /** The lineage as this device holds it (the plain mirror of the synced
     *  registers). */
    internal fun lineageNow(): Lineage {
        val account = currentAccountIdB64.get() ?: return NO_LINEAGE
        return LineageMirror.lineage(applicationContext, account)
    }

    /**
     * "The conversation's current group" (R.5) for the conversation
     * [groupIdHex] belongs to: the last segment this device is an active
     * member of, or `null` when it holds none.
     */
    internal fun headGroupOf(groupIdHex: String): String? {
        val lineage = lineageNow()
        val segments = lineage.segmentsOf(lineage.conversationOf(groupIdHex.lowercase()))
        return headSegment(segments) { hex ->
            when (isGroupActive(hex)) {
                null -> SegmentMembership.ABSENT
                false -> SegmentMembership.ENDED
                true -> SegmentMembership.ACTIVE
            }
        }
    }

    /** R.5 — the conversation id (hex) [groupIdHex] belongs to: the key of
     *  every person-facing store. */
    public fun conversationIdOf(groupIdHex: String): String = lineageNow().conversationOf(groupIdHex.lowercase())

    /** R.5 — the unsent composer text per conversation ([ComposerCarry]). */
    private val composerCarry = ComposerCarry()

    /** The composer of the chat screen on [groupIdHex] now holds [text]. */
    public fun noteComposerText(groupIdHex: String, text: String) {
        composerCarry.note(conversationIdOf(groupIdHex), text)
    }

    /** What the person typed and did not send in the conversation
     *  [groupIdHex] belongs to, whichever of its groups the screen was on. */
    public fun unsentComposerText(groupIdHex: String): String? = composerCarry.textOf(conversationIdOf(groupIdHex))

    /** R.5 — every segment of the conversation [groupIdHex] belongs to that
     *  this device holds, oldest first ([groupIdHex] itself when it stands
     *  alone). */
    public fun conversationSegments(groupIdHex: String): List<String> {
        val lineage = lineageNow()
        val held = lineage.segmentsOf(lineage.conversationOf(groupIdHex.lowercase())).filter { groups.containsKey(it) }
        return held.ifEmpty { listOf(groupIdHex) }
    }

    /** Hold one composed message: its row reads "Sending..." and nothing is
     *  published. A give-up check is scheduled ([HELD_SEND_GIVE_UP_MS]). */
    private fun holdForSuccessor(
        groupIdHex: String,
        text: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef>,
        reply: ReplyRef?,
        forward: ForwardRef?,
        content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    ) {
        val record = groups[groupIdHex] ?: return
        val localId = record.nextLocalId()
        record.appendPending(
            localId = localId,
            text = text,
            attachments = attachments,
            createdAt = java.time.Instant.now().toString(),
            authorKey = null,
            reply = reply,
            forward = forward,
            content = content,
        )
        publishThreadList()
        enqueueHeld(HeldMessage(groupIdHex, localId, System.currentTimeMillis(), text, attachments, reply, forward, content))
    }

    /** The ordinary Retry of a held message that gave up: held again. */
    private fun retryHeld(groupIdHex: String, localId: Long) {
        val record = groups[groupIdHex] ?: return
        val row = record.snapshot().firstOrNull { it.localId == localId } ?: return
        if (row.ack != Ack.FAILED) return
        record.markPending(localId)
        publishThreadList()
        enqueueHeld(
            HeldMessage(groupIdHex, localId, System.currentTimeMillis(), row.text, row.attachments, row.reply, row.forward, row.content),
        )
    }

    private fun enqueueHeld(held: HeldMessage, persist: Boolean = true) {
        val conversation = lineageNow().conversationOf(held.groupIdHex.lowercase())
        heldSends.getOrPut(conversation) { java.util.Collections.synchronizedList(ArrayList()) }.add(held)
        if (persist) persistHeldSends()
        // The identity check and the continuity pass are what can end the
        // wait; a send is one of the rule's triggers (R.2).
        scheduleContinuity(held.groupIdHex)
        pushScope.launch {
            delay(HELD_SEND_GIVE_UP_MS)
            withContext(mlsDispatcher) { settleHeld(conversation) }
        }
    }

    /**
     * Send what can go and fail what waited too long ([heldVerdict]). The
     * route is judged over the conversation's CURRENT group: while that is
     * still the dead one-to-one the route is `HOLD`; once the successor is
     * the head the held messages go out into it, in order, and their rows
     * leave the old segment.
     */
    private suspend fun settleHeld(conversationHex: String) {
        val queue = heldSends[conversationHex] ?: return
        val head = headGroupOf(conversationHex)
        val route = if (head == null) {
            SendRoute.HOLD
        } else {
            sendRoute(conversationKindOf(groupKind(head)), unrepairableKeyOf(head))
        }
        val now = System.currentTimeMillis()
        val pending = synchronized(queue) { queue.toList() }
        for (held in pending) {
            when (heldVerdict(route, held.heldAtMs, now)) {
                HeldVerdict.WAIT -> Unit
                HeldVerdict.GIVE_UP -> {
                    synchronized(queue) { queue.remove(held) }
                    groups[held.groupIdHex]?.markFailed(held.localId)
                    publishThreadList()
                }
                HeldVerdict.SEND -> {
                    val target = head ?: continue
                    synchronized(queue) { queue.remove(held) }
                    try {
                        sendMessageOnMlsThread(
                            groupIdHex = target,
                            plaintext = held.text,
                            attachments = held.attachments,
                            // Rule 7 — the quote is kept; a target that
                            // belongs to another segment is dropped (0).
                            reply = held.reply?.let { r ->
                                r.copy(target = replyTargetForSend(r.target, held.groupIdHex, target) ?: REPLY_NO_JUMP_TARGET)
                            },
                            forward = held.forward,
                            content = held.content,
                        )
                        // It went out in the successor: the held row of the
                        // old segment is not a second copy of it.
                        groups[held.groupIdHex]?.removeLocal(held.localId)
                        publishThreadList()
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (cause: Throwable) {
                        // Not sent: this one and everything after it stay
                        // held, in order, for the next attempt (the next poll
                        // of the conversation), as on web and iOS. The give-up
                        // check still fails one that waits too long.
                        android.util.Log.w("ChatRepository", "ADR-0073 held message not sent yet (${cause.javaClass.simpleName})")
                        synchronized(queue) { if (!queue.contains(held)) queue.add(0, held) }
                        break
                    }
                }
            }
        }
        synchronized(queue) { if (queue.isEmpty()) heldSends.remove(conversationHex, queue) }
        // The durable copy follows what went out or gave up.
        if (synchronized(queue) { queue.toList() } != pending) persistHeldSends()
    }

    /** The refusal of a publish into a dead session. It carries no copy of
     *  its own (R.0): the caller shows its ordinary failed state. */
    private fun deadSessionError(): ChatError =
        ChatError("held for the conversation's successor", IllegalStateException("dead session"))

    /**
     * R.2 -> R.5 to R.7 - the identity gate accepted a reset: every shared
     * conversation is looked at again. Launched; each group is independent
     * and a failure in one is logged, not propagated. Nothing is shown.
     */
    public fun onResetAccepted(event: me.lakko.app.contacts.ResetAccepted) {
        supersededCache = null
        unrepairableCache.clear()
        val groupIds = synchronized(groups) { groups.keys.toList() }
        for (groupIdHex in groupIds) {
            // R.12 - a live one-to-one call in a session that just died ends
            // locally (its `End` frame is not published there).
            if (deadOneToOne(groupIdHex)) {
                try {
                    oneToOneMovedSink?.invoke(groupIdHex)
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0073: call not ended", cause)
                }
            }
            scheduleResetRepair(groupIdHex)
        }
    }

    /**
     * R.2 "Triggers" - opening a conversation: its re-admission slot is made
     * due (the arm runs the identity check of every roster account this
     * device can name, at the tail of the next poll) and the repair an
     * accepted reset still owes this group runs again.
     */
    public fun checkResetsOnOpen(groupIdHex: String) {
        if (conversationKindOf(groupKind(groupIdHex)) == ConversationKind.Chat) {
            DeviceReadmission.clearSlot(groupIdHex)
        }
        scheduleResetRepair(groupIdHex)
    }

    /** Groups whose repair pass is queued or running: one at a time each. */
    private val resetRepairInFlight: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** Run [repairAfterReset] for [groupIdHex] off the caller's path. */
    private fun scheduleResetRepair(groupIdHex: String) {
        if (supersededKeys().isEmpty()) return
        if (!resetRepairInFlight.add(groupIdHex)) return
        pushScope.launch {
            try {
                withContext(mlsDispatcher) { repairAfterReset(groupIdHex) }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0073 repair failed for $groupIdHex", cause)
            } finally {
                resetRepairInFlight.remove(groupIdHex)
            }
        }
    }

    /** This device's ADR-0022 role in [group], as the repair plans read it. */
    private fun ownRoleOf(group: Group, identity: Identity): OwnRole? =
        try {
            when (group.ownRole(identity)) {
                Role.OWNER -> OwnRole.OWNER
                Role.ADMIN -> OwnRole.ADMIN
                Role.MEMBER -> OwnRole.MEMBER
                null -> null
            }
        } catch (_: Throwable) {
            null
        }

    /**
     * R.7, one pass over one group, for every superseded key its roster still
     * names. Case 2 (`REPAIR`): an owner or admin proposes the `Remove` of the
     * old leaves ([repairPlan]; the proposal path fans one named leaf out to
     * every leaf of that account) and the owner restores an admin
     * ([adminRestoreStep]); the `Add` of the new identity's devices is the
     * re-admission arm's ([readmitIfDue]), which is no longer suppressed.
     * Cases 1 and 3 are continued by claim-then-build
     * ([continueConversation]). Case 4 does nothing.
     */
    private suspend fun repairAfterReset(groupIdHex: String) {
        MainThreadGuard.assertOffMain("repairAfterReset")
        val identity = canonicalIdentity.get() ?: return
        val group = groups[groupIdHex]?.group ?: return
        val superseded = supersededKeys()
        if (superseded.isEmpty()) return
        val membershipEnded = readIsActive { group.isActive() } == false
        val (roster, table, ownRole) = providerLock.withLock {
            val leaves = try {
                rosterOf(group).map { PersonLeaf(it.leafIndex, outboxB64(personKeyOf(it))) }
            } catch (_: Throwable) {
                null
            }
            Triple(leaves, roleTableOrNull(group), ownRoleOf(group, identity))
        }
        if (roster == null) return
        val kind = conversationKindOf(groupKind(groupIdHex))
        val ownerKeyB64 = table?.owner?.let { outboxB64(it) }
        val adminKeysB64 = table?.admins?.map { outboxB64(it) }.orEmpty()
        val rosterKeys = roster.map { it.personKeyB64 }.distinct()
        var proposed = false
        for (supersededKeyB64 in rosterKeys.filter { it in superseded }) {
            when (resetCase(kind, ownerKeyB64, supersededKeyB64)) {
                ResetCase.NO_ROLE_TABLE -> Unit
                ResetCase.ONE_TO_ONE, ResetCase.OWNED -> continuityPass(groupIdHex)
                ResetCase.REPAIR -> {
                    val pending = pendingMembershipRequests(groupIdHex)
                    val plan = repairPlan(
                        ownRole = ownRole,
                        roster = roster,
                        supersededKeyB64 = supersededKeyB64,
                        pendingRemoveLeaves = pending
                            .filter { it.kind == PendingRequestKind.REMOVE }
                            .mapNotNull { it.targetLeaf },
                        removeInFlight = false,
                        membershipEnded = membershipEnded,
                    )
                    val leaf = plan.removeLeaf
                    if (leaf != null) {
                        try {
                            removeMemberFromGroup(groupIdHex, leaf)
                            proposed = true
                        } catch (cause: kotlinx.coroutines.CancellationException) {
                            throw cause
                        } catch (cause: Throwable) {
                            // A refusal for a full queue is re-sent when the
                            // queue drains: the next roster change or the
                            // re-admission arm runs this again.
                            android.util.Log.w("ChatRepository", "ADR-0073 repair: remove not proposed", cause)
                        }
                    }
                }
            }
        }
        // The owner restores an admin whose key was retired, one role change
        // per sweep: promote the new key once it is a member, then demote the
        // old one.
        if (ownRole == OwnRole.OWNER) {
            val currentKeyOf = pinnedKeysByAccount(superseded.values.toSet())
            for (supersededKeyB64 in adminKeysB64.filter { it in superseded }) {
                val newKeyB64 = currentKeyOf[superseded[supersededKeyB64]] ?: continue
                val step = adminRestoreStep(
                    ownRole = ownRole,
                    adminKeysB64 = adminKeysB64,
                    supersededKeyB64 = supersededKeyB64,
                    newKeyB64 = newKeyB64,
                    rosterPersonKeysB64 = rosterKeys,
                    roleChangePending = pendingMembershipRequests(groupIdHex)
                        .any { it.kind == PendingRequestKind.ROLE_CHANGE },
                ) ?: continue
                val (key, makeAdmin) = when (step) {
                    is AdminRestoreStep.Promote -> step.keyB64 to true
                    is AdminRestoreStep.Demote -> step.keyB64 to false
                }
                try {
                    setMemberAdmin(groupIdHex, java.util.Base64.getDecoder().decode(key), makeAdmin)
                    proposed = true
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0073 repair: admin not restored", cause)
                }
                break
            }
        }
        if (proposed) sweepIfCommitter(groupIdHex)
    }

    /** The key each of [accountIdsB64] is pinned under now. */
    private fun pinnedKeysByAccount(accountIdsB64: Set<String>): Map<String, String> {
        val pins = me.lakko.app.contacts.PeerPinStore.forBoundAccount(applicationContext) ?: return emptyMap()
        val out = HashMap<String, String>()
        for (id in accountIdsB64) {
            try {
                pins.readPin(id)?.pinnedSignaturePubkeyB64?.let { out[id] = it }
            } catch (_: Throwable) {
                // not readable now: nothing is decided for that account
            }
        }
        return out
    }

    /**
     * R.7 case 2 / UX 16.5 - whether the `Members changed` rows of one poll
     * batch are suppressed: the batch changed which devices the group holds
     * and not which PEOPLE, including when the `Remove` and the `Add` land in
     * two batches ([deviceSwapAcrossCommits], whose memory is kept per group).
     */
    private fun suppressMembersChangedRow(
        groupIdHex: String,
        accountKeysBefore: List<String>?,
        accountKeysAfter: List<String>?,
    ): Boolean {
        val account = currentAccountIdB64.get() ?: return false
        // No early return for an account that never saw a key reset: a commit
        // that only adds or retires DEVICE leaves (ADR-0050) leaves the set of
        // people equal, and that alone suppresses the row.
        val superseded = supersededKeys()
        val awaited = continuityStore.row(account).swapping[groupIdHex].orEmpty()
        val currentToAccount = HashMap<String, String>()
        if (superseded.isNotEmpty()) {
            for ((id, key) in pinnedKeysByAccount(superseded.values.toSet())) currentToAccount[key] = id
        }
        val verdict = deviceSwapAcrossCommits(
            personKeysBefore = accountKeysBefore,
            personKeysAfter = accountKeysAfter,
            accountOfKey = { k -> superseded[k] ?: currentToAccount[k] },
            isSuperseded = { it in superseded },
            awaited = awaited,
            nowMs = System.currentTimeMillis(),
        )
        if (verdict.awaited != awaited) {
            continuityStore.update(account) { it.withAwaitedSwaps(groupIdHex, verdict.awaited) }
        }
        return verdict.suppress
    }

    /**
     * `docs/ux/second-device.md` section 4 — the row of this device's OWN
     * merged commit (the ADR-0045 sweep). The DS never hands a device its own
     * commit back (the echo is dropped), so the named row a receiver writes
     * from the batch is written here, at the commit's DS message id (the
     * ADR-0081 commit route's answer): the people before the sweep against
     * the people after the merge, in a group only.
     */
    private suspend fun recordOwnCommitRowBestEffort(
        record: GroupRecord,
        groupIdHex: String,
        commitMessageId: Long,
        accountKeysBefore: List<String>?,
    ) {
        try {
            val change = providerLock.sessionLock {
                val after = memberAccountKeysOrNull(record.group)
                val suppressed = suppressMembersChangedRow(groupIdHex, accountKeysBefore, after)
                membershipChangeForGroup(groupIdHex, accountKeysBefore, after, suppressed)
            } ?: return
            if (record.appendOwnCommitRow(commitMessageId, java.time.Instant.now().toString(), change)) {
                persistMessageLogBestEffort()
            }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "own commit row not written: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * `docs/ux/second-device.md` section 4 — the people one receive batch
     * added and removed, to be recorded on its last commit row: only for a
     * batch that carries a commit, only in a GROUP (a kind that cannot be read
     * counts as a group, as [conversationKindOf] does), never when the
     * device-swap verdict suppressed the batch, never naming this account.
     */
    private fun membershipRowChange(
        groupIdHex: String,
        events: List<IncomingChatEvent>,
        accountKeysBefore: List<String>?,
        accountKeysAfter: List<String>?,
        swapSuppressed: Boolean,
    ): MembershipChange? {
        if (events.none { it is IncomingChatEvent.System && it.systemKind == SystemKind.Commit }) return null
        return membershipChangeForGroup(groupIdHex, accountKeysBefore, accountKeysAfter, swapSuppressed)
    }

    /** [membershipRowChange] for a change already known to come from a
     *  commit (a received batch, or this device's own merged sweep). */
    private fun membershipChangeForGroup(
        groupIdHex: String,
        accountKeysBefore: List<String>?,
        accountKeysAfter: List<String>?,
        swapSuppressed: Boolean,
    ): MembershipChange? {
        if (conversationKind(groupIdHex) != ConversationKind.Group) return null
        val superseded = if (accountKeysBefore != null && accountKeysAfter != null &&
            accountKeysBefore.toSet() != accountKeysAfter.toSet()
        ) {
            supersededKeys()
        } else {
            emptyMap()
        }
        val currentToAccount = HashMap<String, String>()
        if (superseded.isNotEmpty()) {
            for ((id, key) in pinnedKeysByAccount(superseded.values.toSet())) currentToAccount[key] = id
        }
        return membershipChangeOf(
            personKeysBefore = accountKeysBefore,
            personKeysAfter = accountKeysAfter,
            accountOfKey = { k -> superseded[k] ?: currentToAccount[k] },
            swapSuppressed = swapSuppressed,
            ownPersonKey = ownPersonKeyB64(),
        )
    }

    /**
     * R.6 - the receive rule for a `group_closed` envelope, which since the
     * visible flow was removed has one producer: a successor claim. Called
     * from the poll's locked apply region, so the roster and role-table read,
     * the stored-claim read, the verdict and the write are one serialized
     * read-decide-write against CURRENT state (C-62 sub-invariant 5). The
     * sender is the account key resolved from the MLS sender at receipt
     * (C-68), never anything in the envelope.
     */
    private fun receiveSuccessorClaim(
        group: Group,
        groupIdHex: String,
        messageId: Long,
        senderKey: MemberIdentity?,
        incoming: uniffi.lakko_ffi_uniffi.FfiGroupClosed,
    ) {
        val account = currentAccountIdB64.get() ?: return
        val sender = senderKey ?: return
        val successor = incoming.successorGroupId ?: return
        val table = roleTableOrNull(group)
        val adminKeysB64 = table?.admins?.map { outboxB64(it) }.orEmpty()
        val claim = SuccessorClaim(
            messageId = messageId,
            successorGroupIdB64 = outboxB64(successor),
            senderKeyB64 = sender,
            senderWasAdmin = sender in adminKeysB64,
            adminKeysAtReceiptB64 = adminKeysB64,
        )
        val key = groupIdHex.lowercase()
        val state = successorClaims.load(account, key) ?: NO_CLAIMS
        val superseded = supersededKeys().keys
        invalidateDeadSession(groupIdHex)
        val verdict = claimVerdict(
            kind = conversationKindOf(groupKind(groupIdHex)),
            groupIdB64 = groupB64OfHex(key),
            claim = claim,
            held = state.accepted,
            unrepairableKeyB64 = unrepairableKeyOf(groupIdHex),
            supersededKeys = superseded,
            survivingKeyB64 = ownPersonKeyB64(),
        )
        when (verdict) {
            ClaimVerdict.ACCEPT -> {
                successorClaims.save(account, key, withAcceptedClaim(state, claim))
                noteConversationMoved(account, groupIdHex)
            }
            ClaimVerdict.KEEP_PENDING -> successorClaims.save(account, key, withPendingClaim(state, claim))
            ClaimVerdict.IGNORE_LATER, ClaimVerdict.DROP_INELIGIBLE, ClaimVerdict.DROP_MALFORMED -> Unit
        }
        // Accepted: the conversation continues there. Pending: the claim
        // itself triggers the identity check that may yield a verdict (R.6).
        if (verdict == ClaimVerdict.ACCEPT || verdict == ClaimVerdict.KEEP_PENDING) {
            DeviceReadmission.clearSlot(groupIdHex)
            scheduleContinuity(groupIdHex)
        }
    }

    /**
     * R.6 - judge again the claims kept while this device had no verdict on
     * the reset: the first eligible one in the journal's order becomes the
     * conversation's successor. Under the per-group lock.
     */
    private suspend fun rejudgePendingClaims(groupIdHex: String) {
        val account = currentAccountIdB64.get() ?: return
        val key = groupIdHex.lowercase()
        groupLock(groupIdHex).withLock {
            val state = successorClaims.load(account, key) ?: return
            if (state.accepted != null || state.pending.isEmpty()) return
            val unrepairable = unrepairableKeyOf(groupIdHex) ?: return
            val first = firstEligibleClaim(
                kind = conversationKindOf(groupKind(groupIdHex)),
                groupIdB64 = groupB64OfHex(key),
                claims = state.pending,
                unrepairableKeyB64 = unrepairable,
                supersededKeys = supersededKeys().keys,
                survivingKeyB64 = ownPersonKeyB64(),
            )
            if (first == null) {
                // A verdict exists and none of the kept claims is eligible:
                // they are spent.
                successorClaims.save(account, key, state.copy(pending = emptyList()))
            } else {
                successorClaims.save(account, key, withAcceptedClaim(state, first))
                noteConversationMoved(account, groupIdHex)
            }
        }
    }

    /**
     * R.6 / R.7 cases 1 and 3 — one pass over one OLD group: the identity
     * check of a one-to-one's peer, the re-judging of claims kept without a
     * verdict, the claim-then-build order ([continueConversation] over
     * [continuityPort]), then what follows a decided successor (the old
     * segment's push subscription, the held messages).
     */
    private suspend fun continuityPass(groupIdHex: String) {
        val account = currentAccountIdB64.get() ?: return
        // A one-to-one whose peer this device has no verdict on yet: the
        // identity check is one of the rule's triggers and may accept a reset.
        runIdentityCheckOfPeer(groupIdHex)
        rejudgePendingClaims(groupIdHex)
        val key = groupIdHex.lowercase()
        if (continuityInFlight.add(key)) {
            try {
                var step = continueConversation(continuityPort, key)
                if (step == ContinuityStep.CLAIMED) {
                    // Step 3 waits for the old journal to be read up to the
                    // claim: read it now rather than at the next poll tick.
                    try {
                        pollGroupOnMlsThread(key, waitSecs = 0, joinInFlight = true)
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (_: Throwable) {
                        // the next poll reads it
                    }
                    step = continueConversation(continuityPort, key)
                }
                if (step != ContinuityStep.NOTHING && step != ContinuityStep.ADOPTED) {
                    android.util.Log.i("ChatRepository", "ADR-0073 continuity: $step")
                }
            } finally {
                continuityInFlight.remove(key)
            }
        }
        // A decided successor (the pass above recorded its lineage register,
        // which is write-once and synced to this account's other devices).
        successorClaims.load(account, key)?.accepted?.let { accepted ->
            // Also when the pass could not read the old group's facts: an
            // accepted claim is the conversation's successor regardless.
            try {
                continuityPort.recordLineage(key, groupHexOfB64(accepted.successorGroupIdB64))
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0073 lineage not recorded", cause)
            }
            retireOldSegmentPushIfDue(account, key)
        }
        settleHeld(lineageNow().conversationOf(key))
    }

    /**
     * R.5 — records `successor -> root conversation` in the synced state and
     * tells this account's other devices
     * ([me.lakko.app.devices.sync.DeviceGroupSyncRepository.recordLineage]).
     * Installed by the app shell; `null` (tests) records nothing.
     */
    @Volatile
    public var lineageRecorder: (suspend (successorGroupId: ByteArray, rootConversationId: ByteArray) -> Boolean)? = null

    /** R.2 "Triggers" — the identity check of the other person of a
     *  one-to-one, at most once per [DeviceReadmission.INTERVAL_MS]. */
    private val peerCheckedAt = java.util.concurrent.ConcurrentHashMap<String, Long>()

    private suspend fun runIdentityCheckOfPeer(groupIdHex: String) {
        if (conversationKindOf(groupKind(groupIdHex)) != ConversationKind.Chat) return
        val check = contactIdentityCheck ?: return
        val now = System.currentTimeMillis()
        if (!identityCheckDue(peerCheckedAt[groupIdHex], now, DeviceReadmission.INTERVAL_MS)) return
        peerCheckedAt[groupIdHex] = now
        val account = keyPackageAccount.get() ?: return
        val own = ownPersonKeyB64() ?: return
        val facts = continuityFactsOf(groupIdHex) ?: return
        val superseded = supersededKeys()
        val directory = me.lakko.app.profile.ProfileDirectory.get(applicationContext)
        val candidates = DeviceReadmission.readmissionCandidates(
            rosterPersonKeysB64 = facts.rosterPersonKeysB64,
            ownPersonKeyB64 = own,
            contacts = me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext, account.accountId).map { c ->
                DeviceReadmission.LookupContact(outboxB64(c.request.fromPubkey), c.request.fromAccountId?.let { outboxB64(it) })
            },
            accountOfRetiredKey = { superseded[it] },
            profileAccountIdB64Of = { k ->
                directory.profiles.get(k)?.accountIdHex?.let { hex -> hexToBytes(hex)?.let { outboxB64(it) } }
            },
        )
        for (candidate in candidates) {
            val id = try {
                java.util.Base64.getDecoder().decode(candidate.accountIdB64)
            } catch (_: IllegalArgumentException) {
                continue
            }
            try {
                check(id, candidate.rosterKeyB64)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }

    /**
     * R.7 case 1 — the receive side of the re-send marker for one batch of
     * [groupIdHex]: gathers what [resendVerdict] reads. One function for
     * every in-app receiver, so they cannot disagree on what a marker
     * matches. A batch whose facts cannot be read gets "nothing is held", so
     * every message renders (the marker's fail-safe direction).
     */
    private fun heldResendPredicate(
        groupIdHex: String,
        events: List<IncomingChatEvent>,
    ): ((IncomingChatEvent.Application) -> Boolean)? {
        if (events.none { it is IncomingChatEvent.Application && it.resendOrigin != null }) return null
        val lineage = lineageNow()
        val received = groupIdHex.lowercase()
        val receivedConversation = lineage.conversationOf(received)
        val receivedIndex = lineage.segmentIndexOf(received)
        val own = ownPersonKeyB64()
        val superseded = supersededKeys()
        val current = pinnedKeysByAccount(superseded.values.toSet())
        // A key through its aliases: a retired key names the person's
        // current key.
        val canonical = { k: String -> superseded[k]?.let { current[it] } ?: k }
        return { event ->
            val marker = event.resendOrigin
            try {
                val originHex = marker?.let { groupHexOfB64(it.groupIdB64) }.orEmpty()
                val slot = groups[originHex]
                val row = slot?.snapshot()?.firstOrNull { marker != null && it.messageId == marker.messageId && it.messageId > 0 }
                resendVerdict(
                    marker = marker,
                    receivedConversation = receivedConversation,
                    // A group this device holds no record for is not a
                    // segment it can hold the original in.
                    originConversation = if (slot == null) null else lineage.conversationOf(originHex),
                    originIsOlderSegment = lineage.segmentIndexOf(originHex) < receivedIndex,
                    heldAuthor = heldRowAuthor(
                        direction = when (row?.direction) {
                            null -> null
                            Direction.Outgoing -> HeldRowDirection.OUTGOING
                            Direction.Incoming -> HeldRowDirection.INCOMING
                            Direction.System -> HeldRowDirection.SYSTEM
                        },
                        authorKeyB64 = row?.authorKey,
                        ownPersonKeyB64 = own,
                        canonical = canonical,
                    ),
                    senderAuthor = event.senderKey?.let(canonical),
                ) == ResendVerdict.SKIP
            } catch (_: Throwable) {
                false
            }
        }
    }

    /**
     * R.5 / R.8 — one poll batch of [groupIdHex] without what a superseded
     * sender posted ([withoutSupersededActs]). Called from the poll's locked
     * apply region, right after the batch applied.
     */
    private fun withoutSupersededSenders(
        group: Group,
        groupIdHex: String,
        events: List<IncomingChatEvent>,
    ): List<IncomingChatEvent> {
        val superseded = supersededKeys().keys
        if (superseded.isEmpty() || events.isEmpty()) return events
        val key = groupIdHex.lowercase()
        val account = currentAccountIdB64.get()
        val moved = account != null && try {
            continuityStore.row(account).moved.containsKey(key) || successorClaims.load(account, key)?.accepted != null
        } catch (_: Throwable) {
            false
        }
        val roster by lazy {
            try {
                rosterOf(group)
            } catch (_: Throwable) {
                emptyList()
            }
        }
        return withoutSupersededActs(
            events = events,
            supersededKeys = superseded,
            unrepairableKeyB64 = unrepairableKeyOf(groupIdHex),
            moved = moved,
            personKeyOfLeaf = { leaf -> resolveMemberIdentity(roster, leaf) },
        )
    }

    /**
     * R.12 — where a one-to-one call asked for in [groupIdHex] is placed
     * ([me.lakko.app.calls.oneToOneCallTarget]): the group itself, the
     * conversation's successor when this device holds one that is not itself
     * dead, or nowhere (the call fails before any signalling).
     */
    public fun oneToOneCallTargetFor(groupIdHex: String): me.lakko.app.calls.OneToOneCallTarget {
        val key = groupIdHex.lowercase()
        val head = headGroupOf(key)
        return me.lakko.app.calls.oneToOneCallTarget(
            groupIdHex = key,
            unrepairableKeyB64 = unrepairableKeyOf(key),
            headGroupIdHex = head,
            headUnrepairableKeyB64 = head?.takeIf { it != key }?.let { unrepairableKeyOf(it) },
        )
    }

    /** [oldSegmentPolled] over this device's facts about [groupIdHex]. */
    private fun oldSegmentStillPolled(groupIdHex: String): Boolean {
        val account = currentAccountIdB64.get() ?: return true
        val key = groupIdHex.lowercase()
        val movedAt = continuityStore.row(account).moved[key] ?: return true
        val head = headGroupOf(key)
        return oldSegmentPolled(
            // Until this device holds the successor the old group is still
            // where it is: the head for it.
            isHead = head == null || head == key,
            oneToOne = conversationKindOf(groupKind(groupIdHex)) == ConversationKind.Chat,
            movedAtMs = movedAt,
            nowMs = System.currentTimeMillis(),
        )
    }

    /** R.5 — retire this device's push subscription of an older segment once
     *  the conversation's current group is live here ([retiresOldSegmentPush]). */
    private suspend fun retireOldSegmentPushIfDue(accountIdB64: String, groupIdHex: String) {
        val head = headGroupOf(groupIdHex)
        if (!retiresOldSegmentPush(
                groupHex = groupIdHex,
                headHex = head,
                headActive = head != null && isGroupActive(head) == true,
                alreadyRetired = groupIdHex in continuityStore.row(accountIdB64).pushRetired,
            )
        ) {
            return
        }
        try {
            if (retirePushSubscriptionAfterEviction(groupIdHex)) {
                continuityStore.update(accountIdB64) { it.withPushRetired(groupIdHex) }
            }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0073 old segment: push not retired", cause)
        }
    }

    /** R.5 — this device learned [groupIdHex] is continued elsewhere: the
     *  start of the window in which an old group's journal is still read,
     *  and the end of a one-to-one's. A live one-to-one call there ends. */
    private fun noteConversationMoved(accountIdB64: String, groupIdHex: String) {
        val key = groupIdHex.lowercase()
        val oneToOne = conversationKindOf(groupKind(groupIdHex)) == ConversationKind.Chat
        continuityStore.update(accountIdB64) { it.withMoved(key, System.currentTimeMillis(), oneToOne) }
        if (oneToOne) {
            try {
                oneToOneMovedSink?.invoke(groupIdHex)
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0073: call not ended", cause)
            }
        }
    }

    // ---- R.6 / R.7 cases 1 and 3: founding the successor ----------------

    /** Old groups whose continuity pass is running: one at a time each. */
    private val continuityInFlight: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** Successors this device created and has not made held conversations
     *  yet (R.6 step 1), by hex id. After a restart the handle is loaded
     *  from the MLS snapshot the creation persisted. */
    private val preparedGroups = java.util.concurrent.ConcurrentHashMap<String, Group>()

    /** The pinned key of [accountIdB64] with its proven predecessors, the
     *  pinned key last; `null` when the account is not pinned. */
    private fun pinnedChainOf(ownerAccountIdB64: String, accountIdB64: String): List<String>? {
        val pin = try {
            me.lakko.app.contacts.PeerPinStore.forBoundAccount(applicationContext)?.readPin(accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return null
        val links = try {
            me.lakko.app.contacts.SharedPrefsProvenLinkStore(applicationContext).load(ownerAccountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }
        return provenChainEndingAt(pin.pinnedSignaturePubkeyB64, links)
    }

    /** [keyB64] with its proven predecessors, [keyB64] last. */
    private fun provenChainEndingAt(keyB64: String, links: List<me.lakko.app.contacts.ProvenLink>): List<String> {
        val keys = me.lakko.app.contacts.keysOfPerson(keyB64, links)
        return keys.take(keys.indexOf(keyB64) + 1)
    }

    /** The account id each of [personKeysB64] is CLAIMED or known under (a
     *  contact row, the reset ledger, else the person's own profile). Not a
     *  statement of trust: see [verifiedAccountIdOf]. */
    private fun lookupCandidatesFor(personKeysB64: List<String>): List<DeviceReadmission.LookupCandidate> {
        val account = keyPackageAccount.get() ?: return emptyList()
        val own = ownPersonKeyB64() ?: return emptyList()
        val superseded = supersededKeys()
        val directory = me.lakko.app.profile.ProfileDirectory.get(applicationContext)
        return DeviceReadmission.readmissionCandidates(
            rosterPersonKeysB64 = personKeysB64,
            ownPersonKeyB64 = own,
            contacts = me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext, account.accountId).map { c ->
                DeviceReadmission.LookupContact(outboxB64(c.request.fromPubkey), c.request.fromAccountId?.let { outboxB64(it) })
            },
            accountOfRetiredKey = { superseded[it] },
            profileAccountIdB64Of = { k ->
                directory.profiles.get(k)?.accountIdHex?.let { hex -> hexToBytes(hex)?.let { outboxB64(it) } }
            },
        )
    }

    /**
     * The account id a person's CURRENT devices may be fetched under, with
     * the keys their KeyPackages may be bound to, or `null`. An id is
     * followed only when this device trusts the roster key for it: the pin
     * record, or a binding it verified itself against the directory
     * ([DeviceReadmission.lookupStanding]). A claimed id is first put
     * through the identity check, which records a binding when the directory
     * serves exactly this key; when it does not, nobody is added under it.
     */
    private suspend fun verifiedAccountIdOf(personKeyB64: String): Pair<ByteArray, Set<String>>? {
        val owner = keyPackageAccount.get() ?: return null
        val candidate = lookupCandidatesFor(listOf(personKeyB64)).firstOrNull() ?: return null
        val id = try {
            java.util.Base64.getDecoder().decode(candidate.accountIdB64)
        } catch (_: IllegalArgumentException) {
            return null
        }
        fun standing(): Pair<DeviceReadmission.LookupStanding, List<String>?> {
            val chain = pinnedChainOf(owner.accountIdB64, candidate.accountIdB64)
            return DeviceReadmission.lookupStanding(
                rosterKeyB64 = personKeyB64,
                pinnedChainB64 = chain,
                supersededKeysB64 = supersededKeys().filterValues { it == candidate.accountIdB64 }.keys,
                bindingKeyB64 = if (chain == null) {
                    verifiedBindingStore.read(owner.accountIdB64, candidate.accountIdB64)?.keyB64
                } else {
                    null
                },
            ) to chain
        }
        var verdict = standing().first
        if (verdict != DeviceReadmission.LookupStanding.SAME_PERSON) {
            try {
                contactIdentityCheck?.invoke(id, personKeyB64)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                return null
            }
            verdict = standing().first
        }
        if (verdict != DeviceReadmission.LookupStanding.SAME_PERSON) return null
        return id to addFilterKeysNow(candidate.accountIdB64, personKeyB64)
    }

    /** Read-modify-write of one old group's claim state, under its lock (the
     *  receive rule writes the same row from the poll's locked region). */
    private suspend fun updateClaims(
        oldGroupIdHex: String,
        /** The row must be ON DISK when this returns (it is read back after a
         *  restart to finish what a publish started): a failed write throws. */
        durable: Boolean = false,
        change: (StoredClaims) -> StoredClaims,
    ): StoredClaims? {
        val account = currentAccountIdB64.get() ?: return null
        val key = oldGroupIdHex.lowercase()
        return groupLock(key).withLock {
            val held = successorClaims.load(account, key) ?: NO_CLAIMS
            val next = change(held)
            if (next != held && !successorClaims.save(account, key, next) && durable) {
                throw IllegalStateException("the claim state could not be written")
            }
            next
        }
    }

    /** Publish what a membership call proposed, as the members screen does. */
    private suspend fun sweepAfterMembership(groupIdHex: String) {
        try {
            sweepIfCommitterOnMlsThread(groupIdHex)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0073 continuity: sweep failed", cause)
        }
    }

    /** The concrete port of [continueConversation]. Mirror of the web port
     *  in `chat/BackgroundReceiveContext.tsx`. Every function takes the locks
     *  it needs itself; the pass holds none. */
    private val continuityPort: ContinuityPort = object : ContinuityPort {
        override fun ownPersonKeyB64(): String? = this@ChatRepository.ownPersonKeyB64()

        override fun supersededKeys(): Map<String, String> = this@ChatRepository.supersededKeys()

        override fun currentKeyOf(accountIdB64: String): String? = pinnedKeysByAccount(setOf(accountIdB64))[accountIdB64]

        override fun facts(groupIdHex: String): ContinuityFacts? = continuityFactsOf(groupIdHex)

        override fun claimState(oldGroupIdHex: String): StoredClaims {
            val account = currentAccountIdB64.get() ?: return NO_CLAIMS
            return successorClaims.load(account, oldGroupIdHex.lowercase()) ?: NO_CLAIMS
        }

        override fun cursorOf(oldGroupIdHex: String): Long? = groups[oldGroupIdHex]?.cursor

        override suspend fun newIdentityHasDevices(accountIdB64: String): Boolean {
            val wanted = currentKeyOf(accountIdB64) ?: return false
            return try {
                // Only a device bound to the key this device ACCEPTED counts:
                // the directory is not trusted to say whose devices these are.
                val accepted = addFilterKeysNow(accountIdB64, wanted)
                discovery.fetchKeyPackages(java.util.Base64.getDecoder().decode(accountIdB64))
                    .orEmpty()
                    .any { keyPackageBoundTo(it.keyPackage, accepted) }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                false
            }
        }

        override suspend fun prepareSuccessor(oldGroupIdHex: String, newIdentityAccountIdB64: String): String {
            val identity = canonicalIdentity.get() ?: throw IllegalStateException("no session")
            val kind = groupKind(oldGroupIdHex) ?: throw IllegalStateException("the old group is not held")
            val wanted = currentKeyOf(newIdentityAccountIdB64)
                ?: throw IllegalStateException("no accepted key for the new identity")
            val created = prepareGroupOnMlsThread(
                identity = identity,
                recipientAccountId = java.util.Base64.getDecoder().decode(newIdentityAccountIdB64),
                kind = kind,
                expectedKeyB64 = wanted,
            )
            val successorHex = DsClient.pseudonymHex(created.groupId)
            try {
                // The successor must survive a restart between the claim and
                // the delivery: its MLS state goes to disk now, fail-closed.
                providerLock.withLock { persistSnapshotDurableOrThrow() }
                preparedGroups[successorHex] = created.group
                updateClaims(oldGroupIdHex, durable = true) {
                    it.copy(
                        prepared = PreparedSuccessor(
                            successorGroupIdHex = successorHex,
                            welcomeB64 = outboxB64(created.welcomeEnvelope),
                            targetKeysB64 = created.recipientAccounts.map { k -> outboxB64(k) },
                            delivered = false,
                        ),
                    )
                } ?: throw IllegalStateException("no session")
            } catch (cause: Throwable) {
                preparedGroups.remove(successorHex)
                runCatching { created.group.close() }
                throw cause
            }
            return successorHex
        }

        override suspend fun postClaim(oldGroupIdHex: String, successorGroupIdHex: String): Long {
            val identity = canonicalIdentity.get() ?: throw IllegalStateException("no session")
            val record = groups[oldGroupIdHex] ?: throw IllegalStateException("the old group is not held")
            if (readIsActive { record.group.isActive() } == false) throw IllegalStateException("membership ended")
            val successor = hexToBytes(successorGroupIdHex) ?: throw IllegalStateException("malformed successor id")
            // The own-claim record is on disk before the publish.
            updateClaims(oldGroupIdHex, durable = true) { it.copy(own = OwnClaim(successorGroupIdHex, null)) }
                ?: throw IllegalStateException("no session")
            val envelope = uniffi.lakko_ffi_uniffi.composeGroupClosedEnvelope(
                System.currentTimeMillis().toULong(),
                successor,
            )
            // The claim is the one frame posted into the superseded session.
            val published = broadcastControlToGroup(
                identity = identity,
                groupIdHex = oldGroupIdHex,
                group = record.group,
                envelope = envelope,
                publishKind = PublishKind.CLAIM,
            ) ?: throw IllegalStateException("the claim was not published")
            updateClaims(oldGroupIdHex) { it.copy(own = OwnClaim(successorGroupIdHex, published.messageId)) }
            return published.messageId
        }

        override suspend fun abandonOwnClaim(oldGroupIdHex: String) {
            updateClaims(oldGroupIdHex) { if (it.own == null) it else it.copy(own = null) }
        }

        override suspend fun discardSuccessor(successorGroupIdHex: String) {
            preparedGroups.remove(successorGroupIdHex)?.let { runCatching { it.close() } }
            // The prepared record names a successor nobody will ever join.
            val account = currentAccountIdB64.get() ?: return
            for ((oldHex, state) in successorClaims.loadAll(account)) {
                val prepared = state.prepared ?: continue
                if (prepared.successorGroupIdHex == successorGroupIdHex && !prepared.delivered) {
                    updateClaims(oldHex) { it.copy(prepared = null) }
                }
            }
        }

        override suspend fun settle(oldGroupIdHex: String, winner: SuccessorClaim) {
            val account = currentAccountIdB64.get() ?: return
            var changed = false
            updateClaims(oldGroupIdHex) { state ->
                val held = state.accepted
                val next = if (held != null && held.messageId <= winner.messageId) held else winner
                changed = next != held
                state.copy(accepted = next, pending = emptyList(), own = null)
            }
            if (changed) noteConversationMoved(account, oldGroupIdHex)
        }

        override suspend fun recordLineage(oldGroupIdHex: String, successorGroupIdHex: String) {
            val account = currentAccountIdB64.get() ?: return
            val key = oldGroupIdHex.lowercase()
            noteConversationMoved(account, key)
            val lineage = lineageNow()
            val root = lineage.conversationOf(key)
            val successorBytes = hexToBytes(successorGroupIdHex) ?: return
            val rootBytes = hexToBytes(root) ?: return
            if (successorGroupIdHex == root) return
            // Write-once: a register this device already holds is not
            // written (or sent to the other devices) again at every pass.
            if (lineage.conversationOf(successorGroupIdHex) == root) return
            lineageRecorder?.invoke(successorBytes, rootBytes)
        }

        override suspend fun deliverSuccessor(oldGroupIdHex: String, successorGroupIdHex: String): Boolean {
            val identity = canonicalIdentity.get() ?: return false
            val prepared = claimState(oldGroupIdHex).prepared
            // A device of this account that did not create the successor
            // joins it from the Welcome the founder sent to the account's own
            // mailbox.
            if (prepared == null || prepared.successorGroupIdHex != successorGroupIdHex) {
                return groups[successorGroupIdHex] != null
            }
            if (groups[successorGroupIdHex] == null) {
                val successorBytes = hexToBytes(successorGroupIdHex) ?: return false
                // A restart between the claim and the delivery: the successor
                // is in the MLS snapshot the creation persisted.
                val group = preparedGroups[successorGroupIdHex]
                    ?: providerLock.withLock {
                        try {
                            uniffi.lakko_ffi_uniffi.loadGroup(identity, successorBytes)
                        } catch (_: Throwable) {
                            null
                        }
                    }
                    ?: return false
                registerFoundedGroup(identity, group, successorBytes)
                preparedGroups.remove(successorGroupIdHex)
            }
            if (!prepared.delivered) {
                val welcome = prepared.welcomeB64?.let {
                    try {
                        java.util.Base64.getDecoder().decode(it)
                    } catch (_: IllegalArgumentException) {
                        null
                    }
                } ?: return false
                val targets = prepared.targetKeysB64.mapNotNull {
                    try {
                        java.util.Base64.getDecoder().decode(it)
                    } catch (_: IllegalArgumentException) {
                        null
                    }
                }
                // One write per PERSON. ADR-0078: a person whose mailbox this
                // device holds no verified binding for (the new identity of
                // an accepted reset, before they hand one out) is a recorded
                // no-op - see [me.lakko.app.reach.deliverSuccessorWelcomes].
                me.lakko.app.reach.deliverSuccessorWelcomes(
                    targets = targets,
                    mailboxOf = { accountKey ->
                        try {
                            welcomeMailboxFor(accountKey)
                        } catch (_: me.lakko.app.contacts.MailboxUnknown) {
                            null
                        }
                    },
                    publish = { mailbox -> publishWelcomeToMailbox(mailbox, welcome, successorGroupIdHex) },
                )
                // Kept as the mark "this device founded it"; the Welcome is
                // dropped.
                updateClaims(oldGroupIdHex) {
                    it.copy(prepared = PreparedSuccessor(successorGroupIdHex, null, emptyList(), delivered = true))
                }
            }
            return true
        }

        override suspend fun carryName(oldGroupIdHex: String, successorGroupIdHex: String) {
            val account = currentAccountIdB64.get() ?: return
            val identity = canonicalIdentity.get() ?: return
            val record = groups[successorGroupIdHex] ?: return
            val stored = groupName(oldGroupIdHex) ?: return
            val already = groupName(successorGroupIdHex)
            if (already != null && already.updatedAtMs >= stored.updatedAtMs) return
            // The ORIGINAL stamp, never a fresh clock read: a later rename in
            // either group out-ranks it (ADR-0051).
            val envelope = uniffi.lakko_ffi_uniffi.composeGroupNameEnvelope(
                uniffi.lakko_ffi_uniffi.FfiGroupName(name = stored.name, updatedAtMs = stored.updatedAtMs),
            )
            GroupNameStore.save(applicationContext, account, successorGroupIdHex, stored)
            publishThreadList()
            try {
                broadcastControlToGroup(identity = identity, groupIdHex = successorGroupIdHex, group = record.group, envelope = envelope)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                // The store holds it; the growth re-send carries it.
                android.util.Log.w("ChatRepository", "ADR-0073 continuity: name carry-over send failed", cause)
            }
        }

        override suspend fun addPerson(successorGroupIdHex: String, personKeyB64: String): Boolean {
            if (groups[successorGroupIdHex] == null) return false
            val (accountId, _) = verifiedAccountIdOf(personKeyB64) ?: return false
            return try {
                addMemberToGroupOnMlsThread(
                    groupIdHex = successorGroupIdHex,
                    recipientAccountId = accountId,
                    recipientSignaturePubkey = java.util.Base64.getDecoder().decode(personKeyB64),
                )
                sweepAfterMembership(successorGroupIdHex)
                true
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                // Not resolvable or not reachable now: they keep writing in
                // the old group and the completeness re-check tries again.
                android.util.Log.i("ChatRepository", "ADR-0073 continuity: a member was not added yet (${cause.javaClass.simpleName})")
                false
            }
        }

        // Its own slot name, so the re-check and the ADR-0050 arm of the same
        // group do not consume each other's interval.
        override fun recheckDue(successorGroupIdHex: String): Boolean =
            DeviceReadmission.takeSlot("completeness:$successorGroupIdHex", System.currentTimeMillis())

        override suspend fun setAdmins(successorGroupIdHex: String, adminKeysB64: List<String>) {
            val have = continuityFactsOf(successorGroupIdHex)?.adminKeysB64.orEmpty()
            // One role change per sweep (C-65): one admin per pass.
            val next = adminKeysB64.firstOrNull { it !in have } ?: return
            try {
                setMemberAdminOnMlsThread(
                    groupIdHex = successorGroupIdHex,
                    memberSignaturePubkey = java.util.Base64.getDecoder().decode(next),
                    makeAdmin = true,
                )
                sweepAfterMembership(successorGroupIdHex)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                // A pending role change refuses a second one; the next pass.
                android.util.Log.i("ChatRepository", "ADR-0073 continuity: an admin was not restored yet (${cause.javaClass.simpleName})")
            }
        }

        override suspend fun transferOwnership(successorGroupIdHex: String, newOwnerKeyB64: String, stayAdmin: Boolean) {
            try {
                transferGroupOwnershipOnMlsThread(
                    groupIdHex = successorGroupIdHex,
                    newOwnerSignaturePubkey = java.util.Base64.getDecoder().decode(newOwnerKeyB64),
                    outgoing = if (stayAdmin) OutgoingOwner.ADMIN else OutgoingOwner.MEMBER,
                )
                sweepAfterMembership(successorGroupIdHex)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                // Already pending, or a role change is in the way.
                android.util.Log.i("ChatRepository", "ADR-0073 continuity: the owner was not restored yet (${cause.javaClass.simpleName})")
            }
        }

        override suspend fun resendOwnMessages(oldGroupIdHex: String, successorGroupIdHex: String, accountIdB64: String) {
            val account = currentAccountIdB64.get() ?: return
            val identity = canonicalIdentity.get() ?: return
            val old = groups[oldGroupIdHex] ?: return
            val successor = groups[successorGroupIdHex] ?: return
            val key = oldGroupIdHex.lowercase()
            // The founder alone re-sends, once: another device of this
            // account that joined the successor holds no prepared record.
            val prepared = claimState(key).prepared
            if (prepared == null || prepared.successorGroupIdHex != successorGroupIdHex) return
            if (key in continuityStore.row(account).resent) return
            val signedAtMs = resetLedgerStore.loadAll(account)[accountIdB64]?.resetSignedAtMs ?: return
            val rows = old.snapshot()
            val byId = rows.filter { it.messageId > 0 }.associateBy { it.messageId }
            val ids = messagesToResend(
                rows.filter { it.messageId > 0 }.map { m ->
                    OldSegmentMessage(
                        messageId = m.messageId,
                        own = m.direction == Direction.Outgoing,
                        sentAtMs = GroupRecord.parseIsoMillis(m.createdAt),
                        resendable = !m.deleted && m.call == null && (m.text.isNotEmpty() || m.attachments.isNotEmpty()),
                    )
                },
                signedAtMs,
            )
            val originGroupB64 = groupB64OfHex(key)
            // An interrupted re-send continues after the last message that
            // LANDED, never from the first ([resendRemaining]).
            for (id in resendRemaining(ids, continuityStore.row(account).resendCursor[key])) {
                val m = byId[id] ?: continue
                // The same content, sealed again for the new session, with
                // the stored attachment references (it is the same
                // conversation). The marker names the ORIGINAL group and
                // message id: a device of this account that holds the
                // original shows the message once. Never drawn; and no row is
                // added here either, since this device holds the original.
                // It is the same message: a reply keeps its quote (with no
                // jump target, the id belongs to the old journal) and a
                // forward its attribution and origin ([resendEnvelopeParts]).
                val parts = resendEnvelopeParts(
                    originGroupB64 = originGroupB64,
                    originMessageId = id,
                    reply = m.reply?.let { ResendReply(it.author, it.excerpt) },
                    forward = m.forward?.let { f ->
                        ResendForward(
                            author = f.author,
                            originGroupIdB64 = f.originGroupIdHex?.let(::forwardOriginGroupIdB64From),
                            originMessageId = f.originMessageId,
                        )
                    },
                )
                val envelope = uniffi.lakko_ffi_uniffi.composeChatEnvelope(
                    text = m.text,
                    attachments = m.attachments,
                    parts = uniffi.lakko_ffi_uniffi.ChatEnvelopeParts(
                        replyTo = parts.replyTo,
                        replyAuthor = parts.replyAuthor,
                        replyExcerpt = parts.replyExcerpt,
                        forwardedFrom = parts.forwardedFrom,
                        forwardOriginGroupB64 = parts.forwardOriginGroupB64,
                        forwardOriginMessageId = parts.forwardOriginMessageId,
                        resendOriginGroupB64 = parts.resendOriginGroupB64,
                        resendOriginMessageId = parts.resendOriginMessageId,
                    ),
                )
                runGroupSend(
                    lock = groupLock(successorGroupIdHex),
                    providerLock = providerLock,
                    seal = {
                        chatClient.prepareSend(
                            identity = identity,
                            group = successor.group,
                            plaintext = envelope,
                            persistRatchet = { persistSnapshotDurableOrThrow() },
                        )
                    },
                    publish = { sealed ->
                        chatClient.publishPreparedSend(
                            sealed,
                            wake = sealedWakeFor(successorGroupIdHex, DsWakeKind.MESSAGE),
                        )
                    },
                )
                // Durable before the next one is sealed: a pass that dies
                // here re-sends from the message after this one.
                continuityStore.update(account) { it.withResendCursor(key, id) }
            }
            continuityStore.update(account) { it.withResent(key) }
        }
    }

    /** How often every held group is offered a continuity pass from the tail
     *  of a poll round: an old one-to-one is no longer polled once it moved,
     *  so its own poll cannot be what resumes an interrupted build. Chosen. */
    @Volatile
    private var lastContinuitySweepAtMs = 0L

    /** Offer a continuity pass to every group an accepted reset left
     *  unrepairable or that holds a claim state, at most once per
     *  [CONTINUITY_SWEEP_INTERVAL_MS]. Launched. */
    private fun continuitySweepIfDue() {
        val account = currentAccountIdB64.get() ?: return
        if (supersededKeys().isEmpty()) return
        val now = android.os.SystemClock.elapsedRealtime()
        if (lastContinuitySweepAtMs != 0L && now - lastContinuitySweepAtMs < CONTINUITY_SWEEP_INTERVAL_MS) return
        lastContinuitySweepAtMs = now
        val claimed = try {
            successorClaims.loadAll(account).keys
        } catch (_: Throwable) {
            emptySet()
        }
        for (groupIdHex in synchronized(groups) { groups.keys.toList() }) {
            if (groupIdHex in claimed || unrepairableKeyOf(groupIdHex) != null) scheduleContinuity(groupIdHex)
        }
    }

    /** Run the continuity pass of [groupIdHex] off the caller's path. */
    private fun scheduleContinuity(groupIdHex: String) {
        pushScope.launch {
            try {
                withContext(mlsDispatcher) { continuityPass(groupIdHex) }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0073 continuity pass failed for $groupIdHex", cause)
            }
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
        /** ADR-0073 R.6 — the successor claim: the one thing that may still be
         *  published into a dead one-to-one ([publishAllowed]). */
        publishKind: PublishKind = PublishKind.OTHER,
        /** Walk 3, W3-R2 — `true` (the default) for an automatic send (a
         *  repeat envelope, the knock key, the routing secret, the beacon);
         *  `false` for an envelope the person sent by their own action. */
        automatic: Boolean = true,
    ): SendMessageResult? {
        // ADR-0073 R.7 case 1 — no service envelope (profile, pin, routing
        // secret, beacon) is sent into a dead one-to-one either.
        if (!publishAllowed(deadOneToOne(groupIdHex), publishKind)) return null
        // ADR-0091 phase 2 section 2 — nor by a device behind there: sealed
        // at a stale epoch, nobody could open it. Walk 3, W3-R2 — an automatic
        // send is also held while a read of the journal was refused with no
        // answered read since. Quiet: the caller sees `null` (a skipped send).
        // W3-R5 — the person's own action is held only while the device is
        // suspected or confirmed behind, and then it is REFUSED (a thrown
        // error the action's own failure message shows), never a `null` its
        // caller could take for "sent" and apply locally.
        if (automatic) {
            if (behindHoldsAutomaticSends(groupIdHex)) return null
        } else {
            requirePersonActionAllowed(groupIdHex)
        }
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
        // and `redistributePushRoutingBestEffort`), so no group -> routing
        // edge exists and the global
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
        return withPublishEvidence(groupIdHex) {
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
        pushScope.launchSession {
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
        pushScope.launchSession {
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
     * ADR-0043 — the WIRE-OBSERVED rotation trigger: someone ELSE committed
     * a Remove and this poll batch applied it, so the membership SHRANK.
     *
     * A Commit surfaces from the poll loop carrying no membership delta
     * (surfacing removed leaves from `lakko-mls` would be exact but is an
     * invariant-#3 change to the MLS state machine and needs its own ADR),
     * so the removal is INFERRED from the `members()` snapshot taken either
     * side of the batch. A removal this device swept itself is rotated from
     * the sweep's tail (ADR-0081 amendment: no direct-commit removal remains).
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
        // `group` and `member` keys (the evicted member holds the old ones),
        // then republishes the profile carrying the new grants. Since
        // ADR-0078 section 9 they are the account's one key per class, so the
        // profile is republished in EVERY group. This runs after the Remove
        // commit was applied, so the republished profile is sealed at an
        // epoch the evicted member cannot open (the C-61 shape). Every member
        // does this for its own keys, whatever its role.
        if (me.lakko.app.reach.reachRotationOnShrink(previousMemberKeys, currentMemberKeys, selfEvicted)) {
            reachScope.launchSession {
                try {
                    val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
                    // Republishes in every group through the visibility
                    // listener (the same path a setting change takes).
                    reach.rotateClasses(listOf(me.lakko.app.reach.ReachKind.MEMBER, me.lakko.app.reach.ReachKind.GROUP))
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0060 shrink rotation failed: ${cause.javaClass.simpleName}")
                }
            }
        }
        val role = rotationRole(group, identity)
        pushScope.launchSession {
            try {
                GroupPushRouting.rotateIfMembershipShrank(
                    accountIdB64 = currentAccountIdB64.get() ?: return@launchSession,
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
        pushScope.launchSession {
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
    ): GroupPollOutcome = forSession {
        val outcome = pollGroupOnMlsThread(groupIdHex, waitSecs, joinInFlight)
        // A held message whose send failed is tried again at the next poll of
        // its conversation (ADR-0079 reworked: the hand-over to a person
        // added through their link keeps the rest after a failure).
        val conversation = lineageNow().conversationOf(groupIdHex.lowercase())
        if (heldSends.containsKey(conversation)) withContext(mlsDispatcher) { settleHeld(conversation) }
        outcome
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
        // ADR-0087 section 9 — and for one this device is out of on another
        // device's word: its handle still reads active, nothing here reads it.
        if (groupIdHex.lowercase() in endingGroups) return GroupPollOutcome.UNAVAILABLE

        // ADR-0073 R.5 "the old segment after the move" — a one-to-one's old
        // journal is not read at all once the conversation moved (its only
        // other sender is the superseded identity); a group's is, for the
        // journal retention window, since members who have not moved yet
        // still write there.
        if (!oldSegmentStillPolled(groupIdHex)) return GroupPollOutcome.UNAVAILABLE

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
        // ADR-0091 phase 1 — a held registered commit is read again only when
        // its next attempt is due. Until then this poll does not fetch: it
        // waits out its long-poll time or the attempt time, whichever is
        // first (cancellable like the long poll), and returns with the cursor
        // unchanged, so a hold never spins.
        chatClient.holds.msUntilDue(groupIdHex)?.takeIf { it > 0L }?.let { untilDue ->
            val waitMs = minOf(waitSecs.coerceAtLeast(0).toLong() * 1_000L, untilDue)
            if (waitMs > 0L) kotlinx.coroutines.delay(waitMs)
            return GroupPollOutcome.APPLIED
        }

        // ADR-0057 — the epoch before this batch applied: an owner whose batch
        // advanced it (a member joined or a role changed under someone else's
        // commit) reconciles its registrar appointments at the tail. Plain
        // members no longer "heal" a registration (obligation 6).
        var epochBeforeBatch: ULong? = null
        val bound = callBinding()
        val outcome = runSplitPoll(
            inFlight = record.pollInFlight,
            joinInFlight = joinInFlight,
            lock = groupLock(groupIdHex),
            providerLock = providerLock,
            readCursor = { record.cursor },
            fetch = { after ->
                val first = try {
                    chatClient.fetchGroupJournal(
                        // The record caches the group id, so the network half does
                        // not read the `Group` handle at all.
                        groupId = record.groupId,
                        after = after,
                        waitSecs = waitSecs,
                    )
                } catch (cause: Throwable) {
                    // ADR-0091 phase 2 section 1 — three refused reads in a
                    // row suspect this device (its key left the read tier).
                    if (cause !is kotlinx.coroutines.CancellationException) {
                        noteReadRefusedBestEffort(groupIdHex, record, cause)
                    }
                    throw cause
                }
                rewoundIfRolledBack(groupIdHex, record, after, first)
            },
        ) { after, fetched ->
            val (readFrom, raw) = fetched
            MainThreadGuard.assertOffMain("pollGroup apply")
            // ADR-0087 section 9 — a Welcome replaced this record during the
            // fetch and closed its handle: the rows are read by the new one.
            if (groups[groupIdHex] !== record) return@runSplitPoll
            // A long poll is the call most likely to be out when the account
            // leaves. What it fetched is applied to the group, the reach
            // manager, the profile directory and the per-account stores of the
            // account it was fetched for, or not at all.
            bound.ensureCurrent()
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
            // ADR-0073 R.7 case 2 — the ACCOUNT keys before the batch, so a
            // commit that only swaps a person's devices leaves no row.
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
                // ADR-0091 phase 1b — below `after` only on a rewind.
                readFrom = readFrom,
            )
            // ADR-0073 R.5 / R.8 — nothing a superseded sender posted acts:
            // dropped here, before any sink, row or call route sees it
            // ([supersededActDropped]). MLS frames were applied above.
            val batchEvents = withoutSupersededSenders(record.group, groupIdHex, result.events)
            // ADR-0073 R.7 case 2 — read once: the verdict keeps per-group
            // memory of a reset's devices swapping across two batches.
            val accountKeysAfterPoll = memberAccountKeysOrNull(record.group)
            val swapSuppressed = suppressMembersChangedRow(groupIdHex, accountKeysBeforePoll, accountKeysAfterPoll)
            // R.7 case 1 — one predicate for the rows and for the unread
            // count: a re-send whose original is held is neither.
            val heldResend = heldResendPredicate(groupIdHex, batchEvents)
            record.consumeEvents(
                events = batchEvents,
                callSignalingSink = { messageId, payload, createdAt, senderLeaf, senderKey, senderPersonKey ->
                    routeIncomingCallSignaling(
                        groupIdHex, messageId, payload, createdAt, senderLeaf, senderKey, senderPersonKey,
                        record.group,
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
                groupSettingSink = { senderKey, kind, bytes ->
                    // ADR-0087 — inside the locked apply region (C-62 sub-invariant 5).
                    receiveGroupSettingBestEffort(record.group, groupIdHex, senderKey, kind, bytes)
                },
                groupNameSink = { senderKey, name ->
                    // ADR-0051 — inside the poll's locked apply region, where
                    // the role table read and the store write are serialized
                    // per group (C-62 sub-invariant 5).
                    receiveGroupName(record.group, groupIdHex, senderKey, name)
                },
                groupClosedSink = { messageId, senderKey, closed ->
                    // ADR-0073 R.6 — the same critical section as the group
                    // name: the roster and role-table read, the stored-claim
                    // read, the verdict and the write are one
                    // read-decide-write (C-62 sub-invariant 5).
                    receiveSuccessorClaim(record.group, groupIdHex, messageId, senderKey, closed)
                },
                // UX second-device.md section 4 — a batch of commits that
                // changed the PEOPLE of a group names them on its last commit
                // row; a device change or a one-to-one records nothing.
                membershipChange = membershipRowChange(
                    groupIdHex,
                    batchEvents,
                    accountKeysBeforePoll,
                    accountKeysAfterPoll,
                    swapSuppressed,
                ),
                suppressCommitRows = swapSuppressed,
                heldResend = heldResend,
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
                knockKeySink = { senderKey, knock ->
                    // ADR-0091 phase 2 — the same critical section: the role
                    // table is read as it stands at receipt.
                    receiveKnockKey(record.group, groupIdHex, senderKey, knock)
                },
                groupContactRequestSink = { senderKey, envelope, messageId, createdAt ->
                    receiveGroupContactRequest(groupIdHex, senderKey, envelope, messageId, createdAt)
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
            // State bundle v3 — a message from another own device replaces
            // its display copy.
            settleHeldCopies(groupIdHex, record, batchEvents)
            // ADR-0057 S2 finding 1 — the ownership moved away from this
            // account in this batch: hand the journal root over (scheduled).
            noteOwnerTransition(groupIdHex, ownerBeforeBatch, roleTableOrNull(record.group)?.owner)
            // ADR-0052 decision 3 — a delete that landed takes its message's
            // pins with it (section 4.1: no tombstone in the bar).
            if (batchEvents.any { it is IncomingChatEvent.Op && it.op == MessageOp.Delete }) {
                dropPinsOfDeletedMessages(groupIdHex, record)
            }
            // ADR-0024 — a receipt advanced the watermark: persist it off the lock.
            persistReadWatermarksIfReceipts(groupIdHex, record, batchEvents)
            // The chat list's unread badge: incoming chat messages applied
            // while this chat is not on screen (see [LocalUnreadCounter]).
            localUnread.onIncomingApplied(
                groupIdHex,
                batchEvents.filterIsInstance<IncomingChatEvent.Application>()
                    .filter { heldResend?.invoke(it) != true }
                    .map { it.messageId },
                record.syncedThroughMessageId,
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
                // Section 6.4 — an opened group contact request is stored
                // before the snapshot that consumed its row, so a failure
                // holds the cursor with the row still readable.
                persist = {
                    groupContactRequestsFlush?.invoke()
                    persistSnapshotDurableOrThrow()
                },
                advance = {
                    record.advanceCursor(result.cursor)
                    // ADR-0091 phase 1b — the epoch the snapshot just written
                    // is at, recorded with the cursor it was written for.
                    saveCursorEpochBestEffort(groupIdHex, record)
                },
                onHeld = { cause ->
                    // No identifier: the cursor value and the cause's text are
                    // left out.
                    android.util.Log.w(
                        "ChatRepository",
                        "MLS snapshot persist failed; holding the journal cursor so this " +
                            "batch is re-fetched rather than lost (${cause.javaClass.simpleName})",
                    )
                },
            )
            // The plaintext timeline is written either way — held or advanced —
            // and deliberately AFTER the gate so it records whatever cursor the
            // gate left. It is not MLS state: a stale timeline costs a re-render,
            // never a strand. Skipped for a batch that produced nothing, as
            // before, so an idle long poll still writes no file.
            if (result.events.isNotEmpty()) persistMessageLogBestEffort()
            // ADR-0091 phase 2 section 1 — what this read showed about whether
            // this device is behind (skipped rows, the DS epoch, a drained read).
            observeBehindLocked(groupIdHex, record, raw, result)
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
            // The local group has ended: the backup stops naming it on the
            // strength of an earlier restore.
            if (evictedByBatch) dropRestoredKnownGroupBestEffort(groupIdHex)
            // ADR-0087 section 9 — the one seam: record the end durably here,
            // in the region that read the state. The steps run at the tail.
            if (evictedByBatch) recordEndIfOutLocked(groupIdHex, record)
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
            // ADR-0073 R.7 — a batch that changed the roster re-runs the
            // repair of every accepted reset this group still carries (the
            // next old leaf, the new devices, the admin restore).
            if (membersBeforePoll != null && membersAfterPoll != null && membersBeforePoll != membersAfterPoll) {
                invalidateDeadSession(groupIdHex)
                scheduleResetRepair(groupIdHex)
            }
            publishThreadList()
        }
        // Nothing was fetched or applied by THIS call: the poll that was in
        // flight (or that moved the cursor) runs its own tail. Re-running the
        // sweep prelude and the re-admission check here would only burn the
        // provider lock and FFI reads on a result that cannot have changed.
        if (outcome != SplitPollOutcome.APPLIED) return outcome.toGroupPollOutcome()
        // ADR-0087 section 9 — this batch took the account out of the group:
        // nothing below applies to it. Outside both locks.
        if (groupIdHex.lowercase() in endingGroups) {
            runOwedMembershipEnds()
            return GroupPollOutcome.APPLIED
        }
        // ADR-0091 phase 2 section 7 — a device that came back looks for the
        // messages it missed after each applied poll (throttled to once a
        // minute by its record). A read, not a send: also while behind.
        pushScope.launchSession { lookForGapHistoryBestEffort(groupIdHex) }
        // ADR-0091 phase 2 section 2 — a device behind in this conversation
        // commits, sweeps, registers, repairs, re-admits and reads knocks
        // nowhere here: it asks to be brought back, and keeps reading.
        if (behindStopsActing(groupIdHex)) {
            requestReentryIfDueBestEffort(groupIdHex)
            return GroupPollOutcome.APPLIED
        }
        // Section 7 — a gap bundle this device owes as a re-admitter.
        sendOwedGapBundlesBestEffort(groupIdHex)
        // Section 3 route 3 — the owner hands every member the knock key.
        distributeKnockKeyIfDueBestEffort(groupIdHex)
        // ADR-0057 obligation 3 — outside both locks; launched, best effort,
        // and only when the batch moved the epoch (a no-op for a non-owner).
        val epochAfterBatch = providerLock.sessionLock { runCatching { record.group.epoch() }.getOrNull() }
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
        // ADR-0091 phase 2 section 7 — a replacement the sweep just above
        // merged puts the group at the join epoch: the owed bundle goes now,
        // not at the next poll (by which a later commit may have moved on).
        sendOwedGapBundlesBestEffort(groupIdHex)
        // Walk 2, R4 — a member the sweep just above added holds no knock
        // key yet: sent now rather than at the next poll.
        distributeKnockKeyIfDueBestEffort(groupIdHex)
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
        // ADR-0060 section 6 — an Add of this device's that still waits gets a
        // fresh committer envelope when one is due. After the two steps above,
        // which may have landed the Add or re-requested it; outside both
        // locks, because the broadcast takes the per-group lock itself.
        try {
            resendCommitterEnvelopesIfDue(groupIdHex)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0060 committer envelope pass failed", cause)
        }
        // ADR-0087 sections 3 and 8 — the owner's lease renewal and any owed
        // public / private change. Best effort.
        try {
            sendOwedGroupPhotoIfAny(groupIdHex)
            verifyGroupHandleIfDue(groupIdHex)
            maintainGroupHandleIfDue(groupIdHex)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087 handle maintenance failed", cause)
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
            android.util.Log.w("ChatRepository", "ADR-0050 re-admission check failed: ${errorLogTag(cause)}")
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
        // Section 5 — resume from the saved position, so a decided request
        // is not read (and kept) again after a restart.
        if (!record.knockCursorLoaded) {
            record.knockCursor = maxOf(
                record.knockCursor,
                GroupSettingsStore.knockCursor(applicationContext, currentAccountIdB64.get(), groupIdHex),
            )
            record.knockCursorLoaded = true
        }
        val ownerOrAdmin = providerLock.sessionLock {
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
        // What the pass kept, filed after the locks, then the position saved.
        var keptKnocks: List<OpenedKnock> = emptyList()
        var savedThrough: Long? = null
        groupLock(groupIdHex).sessionLock {
            providerLock.sessionLock {
                // Another pass moved the cursor while this one fetched.
                if (record.knockCursor != after) return@sessionLock
                // Every row through the core's knock gate and never through
                // the ordinary receive path: a knock is an unauthenticated
                // write, and a member could knock a real commit (see
                // `KnockGate.kt`). A refused row is dropped, the cursor moves.
                // ADR-0087 section 5 — a knock row is a KeyPackage sealed to
                // the group's knock key: opened with the seed of the row's own
                // version and kept through `retainJoinRequest`. A full pending
                // set stops the cursor before the row.
                val drained = drainSealedKnockRows(raw.messages, after) { bytes ->
                    openSealedKnock(record, groupIdHex, identity, bytes)
                }
                val events = drained.accepted.map { (row, opened) ->
                    IncomingChatEvent.ExternalJoinRequest(
                        messageId = row.id,
                        joinerSignatureKey = opened.joinerSignatureKey,
                        retained = true,
                        createdAt = row.createdAt,
                    )
                }
                record.consumeEvents(events = events)
                // ADR-0060 §6 — the knock carries the joiner's reply key: kept
                // for the joiner's account (after the locks), so the sweep that
                // admits it can write the Welcome into its closed mailbox.
                keptKnocks = drained.accepted.map { it.second }
                // Section 8 — where each request came from, for the line
                // under it: the row's stamp, and the link's issuer.
                currentAccountIdB64.get()?.let { acct ->
                    for ((row, opened) in drained.accepted) {
                        try {
                            GroupSettingsStore.saveKnockOrigin(
                                applicationContext,
                                acct,
                                groupIdHex,
                                me.lakko.app.ds.hexOf(opened.joinerSignatureKey),
                                KnockOrigin(
                                    askedAtMs = me.lakko.app.ui.chat.parseMessageTimeMs(row.createdAt),
                                    issuerKeyB64 = opened.linkIssuer?.let { java.util.Base64.getEncoder().encodeToString(it) },
                                ),
                            )
                        } catch (_: Throwable) {
                            // The line is informative; a failed write shows none.
                        }
                    }
                }
                record.knockQueueFull = drained.cursor < (raw.messages.lastOrNull()?.id ?: drained.cursor)
                record.lastSeenKnockId = maxOf(record.lastSeenKnockId, raw.messages.lastOrNull()?.id ?: 0L)
                // The retained request set lives in the provider (ADR-0027),
                // durably before the position may be saved past its rows.
                val durable = drained.accepted.isEmpty() || try {
                    persistSnapshotDurableOrThrow()
                    true
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0087 retained requests not saved: ${cause.javaClass.simpleName}")
                    false
                }
                record.knockCursor = drained.cursor
                if (durable) savedThrough = drained.cursor
            }
        }
        // The reply grants, then the position: a restart before either reads
        // the rows again and keeps them again.
        var grantsKept = true
        for (opened in keptKnocks) {
            try {
                keepKnockReplyGrant(groupIdHex, opened)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                grantsKept = false
                android.util.Log.w("ChatRepository", "ADR-0060 a join reply grant not kept: ${cause.javaClass.simpleName}")
            }
        }
        savedThrough?.takeIf { grantsKept }?.let { saveKnockCursorBestEffort(groupIdHex, it) }
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
            val read = providerLock.sessionLock {
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
                reRequest = { intent -> sessionLeg { reRequestIntent(groupIdHex, identity, intent) } },
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
                // The same filter as the add that first proposed it: the
                // directory is not trusted to say whose device this is.
                val own = keyPackageAccount.get()?.accountId?.contentEquals(accountId) == true
                val listed = if (own) {
                    discovery.fetchKeyPackages(accountId)?.filter { ownKeyPackageUsable(it.keyPackage) }
                } else {
                    personDevicesToAdd(accountId, expectedKeyB64 = null)
                } ?: return false
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
                forSession {
                    retireLeafFromGroup(groupIdHex, identity, outboxUnb64(intent.subjectKeyB64))
                } ?: return false
            is RetryableRequest.Leave ->
                forSession {
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
     * named that this device has not joined yet. Nothing else: a device
     * restored from the phrase with an empty chat list shows no label
     * (2026-10-02), and the groups a restored backup named
     * (`backup.BackupRestoreStore`) never feed it.
     */
    public fun isSynchronizing(): Boolean =
        me.lakko.app.devices.StateBundle.showsSynchronizing(
            applicationContext,
            currentAccountIdB64.get(),
            joinedGroupIdsHex(),
        )

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
        forSession { admitDeviceOnMlsThread(deviceKey = deviceKey) }

    private suspend fun admitDeviceOnMlsThread(deviceKey: ByteArray): DeviceFanOut {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped — call bootstrap() first")
        val account = keyPackageAccount.get()
            ?: throw IllegalStateException("admitDevice: no session")
        val found = awaitDeviceKeyPackage(account.accountId, deviceKey)
        // ADR-0074 section 2.3 — "add my own device" obeys the same rule as
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
        // The wait can last minutes. The chats walked below are read now, and
        // must still be the ones of the account that is admitting its device.
        ensureSession()
        return fanOut { groupIdHex ->
            addKeyPackageToGroup(groupIdHex, identity, found, skipIfPresent = true, accountId = account.accountId)
        }
    }

    /**
     * [me.lakko.app.contacts.addFilterKeys] over what this device holds for
     * [accountIdB64] RIGHT NOW (the pin with its proven chain, else the
     * verified binding, and the keys accepted resets retired). Read only: the
     * caller ran the identity gate, or is [personDevicesToAdd].
     */
    private fun addFilterKeysNow(accountIdB64: String, expectedKeyB64: String?): Set<String> {
        val owner = keyPackageAccount.get()
        val chain = owner?.let { pinnedChainOf(it.accountIdB64, accountIdB64) }
        val binding = if (owner != null && chain == null) {
            try {
                verifiedBindingStore.read(owner.accountIdB64, accountIdB64)?.keyB64
            } catch (_: Throwable) {
                null
            }
        } else {
            null
        }
        // A verified binding is followed along its proven steps as a pin is
        // (ADR-0074 section 3.7): the conversation link names the key this
        // person's conversations stay under.
        val bindingChain = binding?.let { key ->
            val links = try {
                owner?.let { me.lakko.app.contacts.SharedPrefsProvenLinkStore(applicationContext).load(it.accountIdB64) }
            } catch (_: Throwable) {
                null
            }
            provenChainEndingAt(key, links.orEmpty())
        }
        return me.lakko.app.contacts.addFilterKeys(
            expectedKeyB64 = expectedKeyB64,
            pinnedChainB64 = chain ?: bindingChain,
            bindingKeyB64 = binding,
            retiredKeysB64 = supersededKeys().keys,
        )
    }

    /**
     * ADR-0074 section 3.5 x ADR-0073 R.2 — the devices of ANOTHER person
     * that may be added to a group, and the one place they are chosen: the
     * user-facing add, a new conversation, a successor build and the
     * ADR-0050 re-request all come here (the re-admission arm runs the same
     * two steps over a whole roster).
     *
     *  1. The identity gate for the account, so a proven rotation is recorded
     *     and a verified binding becomes a pin with its chain
     *     ([me.lakko.app.contacts.gateHeldKeyForAdd]). Its refusal or a
     *     failed fetch changes nothing here: step 2 reads what the gate
     *     wrote, never what it answered.
     *  2. The directory's devices, kept only when their KeyPackage binding
     *     names [expectedKeyB64], a proven predecessor of it or its proven
     *     successor, and never a key an accepted reset retired
     *     ([me.lakko.app.contacts.addFilterKeys]).
     *
     * [expectedKeyB64] `null` means "the key this device holds for the
     * account" (a re-request, which records no person key).
     */
    private suspend fun personDevicesToAdd(
        accountId: ByteArray,
        expectedKeyB64: String?,
    ): List<me.lakko.app.discovery.DeviceKeyPackage> {
        val accountIdB64 = outboxB64(accountId)
        val owner = keyPackageAccount.get()
        if (owner != null) {
            val pinned = pinnedChainOf(owner.accountIdB64, accountIdB64) != null
            val hasBinding = !pinned && try {
                verifiedBindingStore.read(owner.accountIdB64, accountIdB64) != null
            } catch (_: Throwable) {
                false
            }
            if (me.lakko.app.contacts.addRunsGate(pinned, hasBinding, expectedKeyB64)) {
                try {
                    contactIdentityCheck?.invoke(
                        accountId,
                        me.lakko.app.contacts.gateHeldKeyForAdd(pinned, hasBinding, expectedKeyB64),
                    )
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (_: Throwable) {
                    // best-effort: the filter below reads the stores
                }
            }
        }
        val accepted = addFilterKeysNow(accountIdB64, expectedKeyB64)
        if (accepted.isEmpty()) return emptyList()
        return discovery.fetchKeyPackages(accountId)
            .orEmpty()
            .filter { keyPackageBoundTo(it.keyPackage, accepted) }
    }

    /**
     * ADR-0073 R.2 / ADR-0050 — whether [keyPackage]'s device binding names
     * one of [acceptedKeysB64] ([me.lakko.app.contacts.devicesBoundTo]). A
     * KeyPackage that does not parse, and an empty accepted set, are refused.
     */
    private fun keyPackageBoundTo(keyPackage: ByteArray, acceptedKeysB64: Set<String>): Boolean {
        val bound = try {
            (uniffi.lakko_ffi_uniffi.keyPackageAccountKey(keyPackage)
                ?: uniffi.lakko_ffi_uniffi.keyPackageSignatureKey(keyPackage)).let { outboxB64(it) }
        } catch (_: Throwable) {
            null
        }
        return me.lakko.app.contacts.devicesBoundTo(listOf(keyPackage), acceptedKeysB64) { bound }.isNotEmpty()
    }

    /** [DeviceReadmission.ownKeyPackageUsable] for a KeyPackage one of this
     *  account's devices published, against the loaded conversation identity. */
    private fun ownKeyPackageUsable(keyPackage: ByteArray): Boolean {
        val bound = try {
            uniffi.lakko_ffi_uniffi.keyPackageAccountKey(keyPackage)
        } catch (_: Throwable) {
            null
        }
        val conversation = try {
            accountIdentity.get()?.signaturePublicKey()
        } catch (_: Throwable) {
            null
        }
        return DeviceReadmission.ownKeyPackageUsable(bound, conversation)
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
                    // ADR-0074: a device paired after a key rotation first
                    // publishes under the current key and re-publishes under
                    // the conversation key once the state bundle reached it;
                    // only the second is used, so the wait goes on.
                    ?.takeIf { ownKeyPackageUsable(it) }
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
     * answered; the owed removal retries until it does (ADR-0074).
     */
    public suspend fun retireDevice(deviceKey: ByteArray): DeviceFanOut =
        forSession { retireDeviceOnMlsThread(deviceKey = deviceKey) }

    private suspend fun retireDeviceOnMlsThread(deviceKey: ByteArray): DeviceFanOut {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped — call bootstrap() first")
        // ADR-0074 section 2.3 — the directory row is deleted BEFORE this, by
        // the removal operation itself and with the account statement another
        // device's row needs (`DeviceRemovalRunner`, step 3). Nothing is
        // deleted here any more: a session may delete only its own row
        // without that statement.
        return fanOut { groupIdHex -> retireLeafFromGroup(groupIdHex, identity, deviceKey) }
    }

    /**
     * ADR-0074 — the device keys (hex, the roster encoding of this class) in
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
        keyPackageScope.launchSession {
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
        keyPackageScope.launchSession {
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
            // ADR-0091 phase 2 section 2 — a chat this device is behind in
            // takes no proposal from it, nor one whose read was refused with
            // no answered read since (walk 3, W3-R2); skipped without a
            // warning, since the refusal is the stop list working.
            if (behindHoldsAutomaticSends(groupIdHex)) {
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
                android.util.Log.w("ChatRepository", "ADR-0050 device fan-out: chat skipped (${errorLogTag(cause)})")
            }
        }
        return DeviceFanOut(offered, skipped, failed)
    }

    /**
     * ADR-0050 — retire ONE leaf (by its signature key) from [groupIdHex]. Unlike
     * [removeMemberFromGroup], which removes the PERSON, the account's other
     * devices stay. `null` when no leaf holds that key. A proposal; the device
     * that sweeps it rotates the push routing secret (ADR-0043).
     */
    private suspend fun retireLeafFromGroup(
        groupIdHex: String,
        identity: Identity,
        leafKey: ByteArray,
    ): MembershipOutcome? {
        val record = groups[groupIdHex] ?: return null
        val leaf = providerLock.sessionLock { leafIndexOfSignatureKey(record.group, leafKey) } ?: return null
        return proposeRemoveLeaf(groupIdHex, identity, leaf)
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
        val snapshot = providerLock.sessionLock {
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
        // ADR-0073 R.2 "Triggers" — every roster account whose account id this
        // device knows, not only its accepted contacts: a contact row, else
        // the account id the member's own profile claims. A claimed id
        // establishes nothing by itself: the identity check below judges it
        // against the pin or the binding this device verified, and
        // [DeviceReadmission.lookupStanding] decides whether its devices are
        // followed at all.
        val superseded = supersededKeys()
        val directory = me.lakko.app.profile.ProfileDirectory.get(applicationContext)
        val candidates = DeviceReadmission.readmissionCandidates(
            rosterPersonKeysB64 = roster.map { it.personKey },
            ownPersonKeyB64 = ownPerson,
            contacts = me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext, account.accountId).map { c ->
                DeviceReadmission.LookupContact(
                    signaturePubkeyB64 = outboxB64(c.request.fromPubkey),
                    accountIdB64 = c.request.fromAccountId?.let { outboxB64(it) },
                )
            },
            accountOfRetiredKey = { superseded[it] },
            profileAccountIdB64Of = { key ->
                directory.profiles.get(key)?.accountIdHex?.let { hex -> hexToBytes(hex)?.let { outboxB64(it) } }
            },
        )
        val pins = me.lakko.app.contacts.PeerPinStore.forBoundAccount(applicationContext)
        // The key a looked-up account's devices must be bound to, per person
        // key the planner files them under (rule 2 of the mirror brief).
        val expectedKeyOf = HashMap<String, Set<String>>()
        var mayAddReset: Boolean? = null
        for (candidate in candidates) {
            val id = try {
                java.util.Base64.getDecoder().decode(candidate.accountIdB64)
            } catch (_: IllegalArgumentException) {
                continue
            }
            // The arm also fetches and checks the account's published
            // identity, so a member who never starts anything with the person
            // still learns of a reset within the interval. An accepted reset
            // re-pins there; its answer is read back from the stores it
            // writes.
            val identityCheck = contactIdentityCheck?.let { check ->
                try {
                    check(id, candidate.rosterKeyB64)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (_: Throwable) {
                    // best-effort
                    null
                }
            }
            val pin = try {
                pins?.readPin(candidate.accountIdB64)
            } catch (_: Throwable) {
                null
            }
            // Read after the check above: the conversation link it may just
            // have recorded (ADR-0074 section 3.7) counts in this same round.
            val provenLinks = try {
                me.lakko.app.contacts.SharedPrefsProvenLinkStore(applicationContext).load(account.accountIdB64)
            } catch (_: Throwable) {
                emptyList()
            }
            val chain = pin?.let { provenChainEndingAt(it.pinnedSignaturePubkeyB64, provenLinks) }
            val bindingKey = if (pin == null) {
                verifiedBindingStore.read(account.accountIdB64, candidate.accountIdB64)?.keyB64
            } else {
                null
            }
            val standing = DeviceReadmission.lookupStanding(
                rosterKeyB64 = candidate.rosterKeyB64,
                pinnedChainB64 = chain,
                supersededKeysB64 = supersededKeys().filterValues { it == candidate.accountIdB64 }.keys,
                bindingKeyB64 = bindingKey,
                bindingChainB64 = bindingKey?.let { provenChainEndingAt(it, provenLinks) }.orEmpty(),
            )
            if (standing == DeviceReadmission.LookupStanding.SKIP) {
                android.util.Log.i(
                    "ChatRepository",
                    "ADR-0050 re-admission: a member is not followed: check=$identityCheck " +
                        "pinned=${pin != null} chain=${chain?.size}",
                )
            }
            when (standing) {
                DeviceReadmission.LookupStanding.SKIP -> continue
                DeviceReadmission.LookupStanding.SAME_PERSON -> {
                    expectedKeyOf[candidate.rosterKeyB64] =
                        addFilterKeysNow(candidate.accountIdB64, candidate.rosterKeyB64)
                    lookups += candidate.rosterKeyB64 to id
                }
                DeviceReadmission.LookupStanding.RESET_IDENTITY -> {
                    // R.7 case 2 — the directory now lists the NEW identity's
                    // devices. Only an owner or an admin proposes their Add
                    // (the core refuses a plain member's), and only in a
                    // group that is repaired in place.
                    val newKey = pin?.pinnedSignaturePubkeyB64 ?: continue
                    val may = mayAddReset ?: (
                        mayAddResetIdentity(providerLock.withLock { ownRoleOf(record.group, identity) }) &&
                            unrepairableKeyOf(groupIdHex) == null
                        ).also { mayAddReset = it }
                    if (!may) continue
                    expectedKeyOf[newKey] = addFilterKeysNow(candidate.accountIdB64, newKey)
                    lookups += newKey to id
                }
            }
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
                    DeviceReadmission.DirectoryDevice(
                        hex,
                        it.keyPackage,
                        // ADR-0074: an own device's KeyPackage bound to another
                        // key than the conversation key is not used. ADR-0073
                        // R.2: nor is another account's device whose binding
                        // names a key that is not the expected person's.
                        admissible = if (person == ownPerson) {
                            ownKeyPackageUsable(it.keyPackage)
                        } else {
                            keyPackageBoundTo(it.keyPackage, expectedKeyOf[person].orEmpty())
                        },
                    )
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                null
            }
            DeviceReadmission.AccountDevices(person, devices)
        }
        val plan = DeviceReadmission.plan(
            roster = roster,
            ownPersonKey = ownPerson,
            accounts = accounts,
            removed = DeviceReadmission.removed(applicationContext, account.accountIdB64, groupIdHex),
            // ADR-0074 — own devices only from the DeviceGroup roster.
            ownDeviceRoster = ownDeviceRoster(),
        )
        // ADR-0073 R.6 / R.7 — the same cadence re-judges kept claims and
        // continues a conversation that cannot be repaired in place.
        if (supersededKeys().isNotEmpty()) {
            scheduleResetRepair(groupIdHex)
        }
        android.util.Log.i(
            "ChatRepository",
            "ADR-0050 re-admission check: candidates=${candidates.size} followed=${lookups.size - 1} " +
                "unreadable=${accounts.count { it.devices == null }} admit=${plan.admit.size} retire=${plan.retire.size}",
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
                android.util.Log.w("ChatRepository", "ADR-0050 re-admission: add not proposed: ${errorLogTag(cause)}")
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
                android.util.Log.w("ChatRepository", "ADR-0050 re-admission: retire not proposed: ${errorLogTag(cause)}")
            }
        }
        if (proposed > 0) {
            android.util.Log.i(
                "ChatRepository",
                "ADR-0050 re-admission: ${plan.admit.size} to admit, ${plan.retire.size} to retire in a conversation",
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
        forSession { pollAllGroupsOnMlsThread(waitSecs = waitSecs, exclude = exclude) }

    /**
     * [pollAllGroups] for the feed's pull to refresh (`docs/ux/feed.md`
     * section 5): the same best-effort pass, reporting how many groups it
     * polled and how many answered, so the feed can tell "the network is not
     * there" from one bad group.
     */
    public suspend fun pollAllGroupsReporting(): me.lakko.app.feed.SourceCheck =
        forSession { pollAllGroupsOnMlsThread(waitSecs = 0) }

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
        val active = groupIds.filter { isGroupActive(it) != false && it.lowercase() !in endingGroups }
        // ADR-0087 section 9 — an end that stopped on a failed step, and a
        // Welcome kept for a group that may have ended since, are retried by
        // every sweep.
        if (endingGroups.isNotEmpty()) runOwedMembershipEnds()
        resolveHeldWelcomes()
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
        // ADR-0073 R.6 — an interrupted build of a successor resumes here.
        continuitySweepIfDue()
        return me.lakko.app.feed.SourceCheck(tried = active.size, answered = answered)
    }

    /**
     * Clear the in-memory state for the current session. Keeps
     * the on-disk MLS snapshot intact — `forgetDevice` is the
     * full-wipe path. Called from `AuthRepository.logout`.
     */
    public fun clear() {
        // First: everything launched for this account stops at its next
        // suspension, and a bootstrap still restoring it stands down.
        sessionBinding.advance()
        // Then the stores: a write already past its "which account?" read on
        // another thread finishes before this returns, and none starts after
        // it (a thread race the binding cannot see). Before any other lock
        // is taken here; it waits for disk writes in flight, never for the
        // network, so it is safe on the main thread this is called from.
        currentAccountIdB64.get()?.let { me.lakko.app.account.AccountStoreGate.seal(it) }
        me.lakko.app.translation.TranslationCenter.unbind()
        introductionTicker?.cancel()
        introductionTicker = null
        me.lakko.app.reach.ReachManager.peek()?.clear()
        DeviceReadmission.clearSlots()
        wipeInMemoryState()
        supersededCache = null
        unrepairableCache.clear()
        resetRootHandoverState()
        // Keyed by group id alone, so the next account on this phone that is
        // a member of the same group would inherit them.
        supersededCheckedEpoch.clear()
        endingGroups.clear()
        appointmentAttemptAtMs.clear()
        undeliveredWelcomeGroups.clear()
        committerEnvelopeNothingToSend.forgetAll()
        lastBeaconAtMs.clear()
        localUnread.clear()
        // ADR-0091 phase 2 — in-memory state of the session that left.
        me.lakko.app.ds.DsClient.groupRowAcceptedListener = null
        conversationBehind.unbind()
        reentryLedger.clear()
        // The owed gap bundles and the gap looks are durable: they stay for
        // the next landing of this account and go on Forget device.
        knockKeySentMarks.clear()
        mutableReentryGaps.value = emptyMap()
        keyPackageAccount.set(null)
        everyoneCheckedThisSession.set(false)
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
            // Not bound to the session: it is the departure itself, and
            // [clear] just below ends the binding.
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
        // The repository is done: its background work (the lineage collector
        // started at construction, the push legs) goes with it.
        pushScope.cancel()
        if (me.lakko.app.devices.HistoryBundleCodec.liveLogSink === historyBundleSink) {
            me.lakko.app.devices.HistoryBundleCodec.liveLogSink = null
        }
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

    /**
     * [bootstrap] found its binding gone after it had installed
     * [accountIdB64]: undo the install. What [clear] does, without ending the
     * binding again (the one current now may already be the next account's,
     * waiting on the provider lock this runs under).
     */
    private fun standDownStaleBootstrap(accountIdB64: String, publishAccount: KeyPackageAccount) {
        wipeInMemoryState()
        keyPackageAccount.compareAndSet(publishAccount, null)
        snapshotUnreadable.set(false)
        currentAccountIdB64.compareAndSet(accountIdB64, null)
        accountIdentity.getAndSet(null)?.close()
        feedRefresh.reset()
        mutableState.value = ChatRepositoryState()
    }

    private fun wipeInMemoryState() {
        synchronized(groups) {
            messageLogAccount = null
            waitingLogSlots.clear()
            for (rec in groups.values) rec.group.close()
            groups.clear()
            for (g in preparedGroups.values) runCatching { g.close() }
            preparedGroups.clear()
            heldSends.clear()
            pendingInvitedHeld.clear()
            heldCopyRows.clear()
        }
        // An unsent text belongs to the session that typed it.
        composerCarry.clear()
        // G-37 — drop the per-group mutexes alongside the records they guard,
        // so an account switch does not carry a previous session's lock map.
        // Safe here: `wipeInMemoryState` runs on the bootstrap / logout path,
        // where there is no in-flight chat operation to strand — and a caller
        // that somehow still held one would simply hold the last reference to
        // an orphaned Mutex, never a lock a NEW operation could also acquire.
        groupLocks.clear()
        // The rows this session published and has not polled back are its own
        // echoes, not the next account's (two accounts on one phone can be
        // members of the same group).
        if (chatClientLazy.isInitialized()) chatClient.forgetOwnEchoes()
        // ADR-0053 — the journal and mailbox signers belong to the session.
        dsSigners.clearOwner(DS_SIGNER_OWNER)
        // ADR-0048 — per-group republish timing belongs to the session.
        profileRepublish.reset()
        // ADR-0051 — and the group-name re-send timing with it.
        groupNameRepublish.reset()
        groupSettingsRepublish.reset()
        knockSeedsSent.clear()
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
        // This device holds the group now, so its own list names it.
        dropRestoredKnownGroupBestEffort(groupIdHex)
    }

    /**
     * A backup always lists the conversations: the ids a restored backup
     * named are kept (`backup.BackupRestoreStore`) until this device speaks
     * for the group itself. Called when it joins the group, when the chat is
     * deleted and when the local group has ended. Best-effort: a row left
     * behind is dropped at the next upload for any group this device holds.
     */
    private fun dropRestoredKnownGroupBestEffort(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            me.lakko.app.backup.BackupRestoreStore.removeRestoredGroup(
                applicationContext,
                accountIdB64,
                groupIdHex,
            )
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
        } catch (ended: me.lakko.app.StaleBindingException) {
            // Not a failed write: the account's session has ended and its
            // store refused it ([me.lakko.app.account.AccountStoreGate]).
            // Passed on, because several callers publish right after this
            // (a membership proposal takes its handshake-ratchet step in
            // memory, persists here and then goes to the wire): swallowed,
            // the step would be published with nothing on disk behind it,
            // and the account's next sign-in would take the same step again.
            throw ended
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

    /**
     * The FAIL-CLOSED persist a membership proposal runs between creating the
     * proposal and publishing it (role change, ownership transfer, add,
     * remove, approve of a join request): [persistSnapshotDurableOrThrow],
     * then the message log best-effort as [persistSnapshotBestEffort] does.
     *
     * The proposal is in this device's own pending store, which is provider
     * state, and its handshake step was taken in memory. Swallowing a failed
     * or refused snapshot here used to send it anyway: a request on the wire
     * that this device did not record, taken again after a restart. A session
     * that ended is passed on as the cancellation it is; any other failure
     * ends the request with nothing published.
     */
    private fun persistProposalOrThrow() {
        try {
            persistSnapshotDurableOrThrow()
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: Throwable) {
            throw ChatError("the proposal could not be recorded on this device; nothing was sent", cause)
        }
        persistMessageLogBestEffort()
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
    /**
     * A write to an outbox entry named by its id alone (ack, discard): made
     * for the signed-in account and inside its store gate, so it neither
     * starts after the session ended nor rewrites an entry the forget-device
     * wipe is deleting.
     */
    private inline fun outboxWrite(write: () -> Unit) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) { write() }
    }

    private fun ackCommitBestEffort(id: String, commitMessageId: Long?, secondMessageId: Long?) {
        try {
            outboxWrite {
                CommitOutboxStore.markAcked(
                    applicationContext,
                    id,
                    commitMessageId,
                    secondMessageId,
                )
            }
        } catch (_: Throwable) {
            // best-effort — the next sweep reconciles.
        }
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
                    return providerLock.sessionLock { journalRepairRankFor(record, groupIdHex) }
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

                // ADR-0091 phase 1 — a held registered commit, or a behind
                // mark at or above the noted epoch, means the epochs above
                // are real: no Replace.
                override fun registeredCommitKnownAbove(groupIdHex: String, epoch: ULong): Boolean =
                    chatClient.holds.repairBlocked(groupIdHex, epoch) ||
                        // ADR-0091 phase 2 section 2 — evidence of real
                        // progress above: a Replace would cut real epochs.
                        conversationBehind.realProgressEvidence(groupIdHex)

                override fun currentEpochKey(groupIdHex: String): me.lakko.app.ds.EpochKey? =
                    currentJournalSigner(groupIdHex)?.let { me.lakko.app.ds.EpochKey(it.epoch, it.verifyingKey) }

                override suspend fun repairSigner(groupIdHex: String): me.lakko.app.ds.DsStatementSigning? {
                    val record = groups[groupIdHex] ?: return null
                    return providerLock.sessionLock {
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
        val (root, epoch, epochKey) = providerLock.sessionLock {
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
        // "This account holds the root of this journal" is written to disk
        // under whoever is signed in; it is true of the account that claimed.
        ensureSession()
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
        // The gate launches the claim on a scope of its own: it carries the
        // binding it was started under.
        val work = SessionWork(sessionBinding.capture())
        val claim = ownerClaimGate.start(groupIdHex, pushScope) {
            try {
                withContext(work) { claimJournalAsOwner(groupIdHex) }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0057: claim of a group journal did not land", cause)
                false
            }
        }
        pushScope.launchSession {
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
        appointmentLocks.getOrPut(groupIdHex) { Mutex() }.sessionLock {
            val snapshot = providerLock.sessionLock {
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
                        // Walk 3, W3-R2 — a held send is not a delivery.
                        broadcastControlToGroup(identity, groupIdHex, record.group, envelope)
                            ?: throw AutomaticSendHeld()
                    },
                    devicesOf = devicesOf,
                )
                // The appointment record is this account's; one that signed
                // out (or forgot the device) while the statements were out
                // gets nothing written back.
                ensureSession()
                result.sentSet?.let { sentRegistrarSets[groupIdHex] = it }
                store.put(accountIdB64, groupIdHex, result.delivered)
                // ADR-0087 section 5 — and the knock seed to each admin.
                deliverKnockSeedsBestEffort(identity, record, groupIdHex, table.admins)
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
            ensureSession()
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
        // ADR-0091 phase 2 section 2 — no registration by a device behind.
        if (behindStopsActing(groupIdHex)) return
        pushScope.launchSession {
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
        pushScope.launchSession {
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
        val active = providerLock.sessionLock { readIsActive { record.group.isActive() } }
        if (active == false) {
            // Evicted: there is no group left to post the seed into.
            clearHandoverOwed(accountIdB64, groupIdHex)
            return
        }
        // The callbacks below read the account identity when they RUN, which
        // is after the probes. The root seed that is sealed and sent is the
        // one of the account that owes the handover, never of an account that
        // signed in while it was out.
        val bound = callBinding()
        val outcome = me.lakko.app.ds.runOwedHandover(
            probeOwnRoot = {
                val root = rootStatementSigner(record.groupId) ?: throw IllegalStateException("no root signer")
                me.lakko.app.ds.probeRoot(record.groupId, root) { make -> dsClient.putStatement(make) }
            },
            lastSentAtMs = { handoverSentAtMs[groupIdHex] },
            nowMs = System::currentTimeMillis,
            send = {
                me.lakko.app.ds.sendRootHandover(
                    ownAccountKey = {
                        bound.ensureCurrent()
                        ownAccountKey()
                    },
                    liveOwner = { providerLock.sessionLock { roleTableOrNull(record.group)?.owner } },
                    ownerX25519 = { accountKey -> memberX25519KeyOrNull(accountKey) },
                    rootSeed = {
                        bound.ensureCurrent()
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
                    sendEnvelope = { envelope ->
                        // Walk 3, W3-R2 — a held send is not a handover sent.
                        broadcastControlToGroup(identity, groupIdHex, record.group, envelope)
                            ?: throw AutomaticSendHeld()
                    },
                )
            },
            log = { line -> android.util.Log.i("ChatRepository", "ADR-0057: $line") },
        )
        bound.ensureCurrent()
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
     * ADR-0074 section 3.5, "DS" - for every group journal this account
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
                    // ADR-0087 sections 3 and 5 — the moved root moves every
                    // knock seed and the handle key: owed steps of the rotation.
                    moveKnockAndHandleOnRotation(accountIdB64, groupIdHex, groupId, oldAccountSeed)
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
            android.util.Log.i("ChatRepository", "ADR-0074: ${owned.size} owned journal root transfer(s) scheduled")
            owned.size
        }

    /**
     * ADR-0087 section 5 (added 2026-11-03) and section 3 — on the owner's
     * ADR-0074 rotation: versions up to the current `v` stay derived from the
     * predecessor's root (kept as stored seeds), the version is bumped so the
     * next and later ones derive from the new root, the boundary is recorded,
     * the new seed goes to the admins, a public group's record is republished
     * (owed), and the held @name owes a `rekey` signed with the old handle
     * seed. Idempotent: a re-run for the same predecessor changes nothing.
     * Throws when a write did not land, so the caller keeps its step owed.
     */
    private fun moveKnockAndHandleOnRotation(
        accountIdB64: String,
        groupIdHex: String,
        groupId: ByteArray,
        oldAccountSeed: ByteArray,
    ) {
        val oldRoot = uniffi.lakko_ffi_uniffi.dsJournalRootSeed(oldAccountSeed, groupId)
        try {
            val ctx = applicationContext
            val boundary = GroupSettingsStore.knockBoundary(ctx, accountIdB64, groupIdHex)
            if (boundary != null) {
                val stored = GroupSettingsStore.knockSeed(ctx, accountIdB64, groupIdHex, boundary)
                val derived = uniffi.lakko_ffi_uniffi.groupKnockSeed(oldRoot, boundary)
                val applied = stored != null && stored.contentEquals(derived)
                stored?.fill(0)
                derived.fill(0)
                if (applied) return
            }
            val current = GroupSettingsStore.knockVersion(ctx, accountIdB64, groupIdHex)
            for (v in 0..current.toInt()) {
                val version = v.toUShort()
                val held = GroupSettingsStore.knockSeed(ctx, accountIdB64, groupIdHex, version)
                if (held != null) {
                    held.fill(0)
                    continue
                }
                val seed = uniffi.lakko_ffi_uniffi.groupKnockSeed(oldRoot, version)
                try {
                    GroupSettingsStore.saveKnockSeed(ctx, accountIdB64, groupIdHex, version, seed)
                } finally {
                    seed.fill(0)
                }
            }
            GroupSettingsStore.saveKnockBoundary(ctx, accountIdB64, groupIdHex, current)
            if (current < UShort.MAX_VALUE) {
                GroupSettingsStore.saveKnockVersion(ctx, accountIdB64, groupIdHex, (current + 1u).toUShort())
            }
            GroupSettingsStore.saveKnockSelfOwed(ctx, accountIdB64, groupIdHex, true)
            // A rotation changes every seed: every recipient gets all of them.
            forgetKnockSeedMarks(groupIdHex)
            val handle = GroupSettingsStore.handle(ctx, accountIdB64, groupIdHex)
            if (handle != null) {
                val oldHandleSeed = uniffi.lakko_ffi_uniffi.handleWriteSeed(oldRoot)
                try {
                    GroupSettingsStore.saveOwedRekeySeed(ctx, accountIdB64, groupIdHex, oldHandleSeed)
                } finally {
                    oldHandleSeed.fill(0)
                }
                if (handle.public) {
                    GroupSettingsStore.saveOwedTypeChange(ctx, accountIdB64, groupIdHex, true to handle.handle)
                }
            }
            // The store's writes report nothing: read the bump back.
            if (GroupSettingsStore.knockBoundary(ctx, accountIdB64, groupIdHex) != current) {
                throw IllegalStateException("the knock boundary was not kept")
            }
        } finally {
            oldRoot.fill(0)
        }
        reconcileAppointmentsBestEffort(groupIdHex, force = true)
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
        // Captured here, with the seed: the takeover transfers the journal's
        // root to "this account's" root key, and which account that is must
        // not change under it (see [runRootTakeover]).
        val bound = sessionBinding.capture()
        pushScope.launchBound(sessionBinding, start = kotlinx.coroutines.CoroutineStart.ATOMIC) {
            try {
                val outcome = runRootTakeover(groupIdHex, seed, bound) ?: return@launchBound
                bound.ensureCurrent()
                if (currentAccountIdB64.get() != accountIdB64) return@launchBound
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
    private suspend fun runRootTakeover(
        groupIdHex: String,
        oldRootSeed: ByteArray,
        bound: me.lakko.app.Binding,
    ): me.lakko.app.ds.TakeOverOutcome? {
        bound.ensureCurrent()
        val record = groups[groupIdHex] ?: return null
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        // Every callback that reads the account identity, or writes this
        // account's ownership records, runs after a network step of the
        // takeover and asks [bound] first. Without it a sign-out and another
        // sign-in in between would have the OLD owner's root seed transfer
        // the journal to the root key of whoever is signed in by then.
        fun <T> forBound(read: () -> T): T {
            bound.ensureCurrent()
            return read()
        }
        val outcome = me.lakko.app.ds.takeOverJournal(
            journalId = record.groupId,
            oldRootSeed = oldRootSeed,
            ownRootPublicKey = { forBound { rootStatementSigner(record.groupId)?.publicKey } },
            stillOwner = { providerLock.sessionLock { forBound { ownsJournal(record.group) } } },
            oldRootTransfer = { seed, newRoot ->
                uniffi.lakko_ffi_uniffi.dsStatementSigner(seed).use { old ->
                    old.rootTransfer(record.groupId, newRoot).let { me.lakko.app.ds.DsStatement(it.path, it.bodyJson) }
                }
            },
            newRoot = { forBound { rootStatementSigner(record.groupId) } },
            desiredRegistrars = {
                providerLock.sessionLock {
                    val table = roleTableOrNull(record.group) ?: return@sessionLock null
                    val roster = rosterLeavesOrNull(record.group) ?: return@sessionLock null
                    me.lakko.app.ds.desiredRegistrars(
                        memberAccounts = roster.map { it.accountKey },
                        ownerAccountKey = table.owner,
                        admins = table.admins,
                    )
                }
            },
            newRegistrarPublicKey = { accountKey ->
                bound.ensureCurrent()
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
            markRootHeld = { forBound { markRootHeld(groupIdHex) } },
            put = { make ->
                bound.ensureCurrent()
                dsClient.putStatement(make)
            },
            resetAppointments = {
                bound.ensureCurrent()
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
     * ADR-0053 client contract point 2 — the mailbox this session owns: the
     * ACCOUNT's (contact requests, acks and Welcomes arrive there). ADR-0078:
     * its key derives from the account seed (`dsMailboxSigner`), the claim
     * registers that key, and there is no device mailbox any more (none is
     * claimed or read). Installs the signer so every read is signed, and
     * claims it. Called at every bootstrap, i.e. after every signup, login
     * and restore.
     */
    private fun claimOwnMailboxesBestEffort() {
        // ADR-0074 part 3: and the mailbox of the PUBLISHED account key when
        // it differs from the conversation identity's (after a rotation).
        //
        // ADR-0074 section 3.5a rule 4: the conversation identity's mailbox
        // is then a PREDECESSOR's: it is read (hence signed for) only for
        // 30 days after this device adopted the new key, and nothing is
        // registered there. Past the window it is left alone.
        val published = publishedAccountIdentity.get()
        val conversation = accountIdentity.get()?.takeIf { published == null || predecessorMailboxStillRead(it) }
        // ADR-0078: account identities only (a predecessor's mailbox derives
        // from the predecessor's seed); the device identity has no mailbox.
        val identities = ownMailboxIdentities(conversation, published)
        for (identity in identities) {
            val (pseudonym, signer) = try {
                val ffi = uniffi.lakko_ffi_uniffi.dsMailboxSigner(identity)
                ffi.mailboxAddress() to FfiDsJournalSigner(ffi)
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0053: mailbox signer unavailable", cause)
                continue
            }
            dsSigners.installStatic(dsMailboxKey(pseudonym), signer, owner = DS_SIGNER_OWNER)
            pushScope.launchSession {
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
     *  identities) is inside its read window (ADR-0074 3.5a rule 4). An
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

    private fun usesProposalPath(group: Group): Boolean = roleTablePresent { group.groupRoles() }

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
     * ADR-0090 section 1 - a channel's PublisherGroup leaf is this device's
     * bound identity, in this provider: the one whose KeyPackage the
     * directory serves, so the Welcome an owner device writes after admitting
     * this device opens here. Every call runs under [providerLock] with no
     * network inside, as the chat regions do; the snapshot is the chat one.
     */
    public val channelDevicePort: me.lakko.app.channels.ChannelDevicePort =
        object : me.lakko.app.channels.ChannelDevicePort {
            override fun deviceFor(accountIdB64: String): Identity? {
                val bound = currentAccountIdB64.get() ?: return null
                if (me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(bound) !=
                    me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
                ) {
                    return null
                }
                val identity = canonicalIdentity.get() ?: return null
                // An unbound fallback identity (no device seed) cannot hold a
                // PublisherGroup leaf: the core refuses it.
                return identity.takeIf { runCatching { it.deviceBinding() }.getOrNull() != null }
            }

            override suspend fun <T> locked(device: Identity, block: (Identity) -> T): T =
                withContext(mlsDispatcher) {
                    providerLock.sessionLock {
                        check(canonicalIdentity.get() === device) { "the device identity changed" }
                        MainThreadGuard.assertOffMain("ChatRepository.channelDevicePort")
                        block(device)
                    }
                }

            override suspend fun persistDurably(device: Identity) {
                withContext(mlsDispatcher) {
                    providerLock.sessionLock {
                        check(canonicalIdentity.get() === device) { "the device identity changed" }
                        persistSnapshotDurableOrThrow()
                    }
                }
            }
        }

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
     * else the conversation whose kind is one-to-one (ADR-0062) and whose
     * people are exactly this account and that person, else `null`. Never a
     * group that happens to hold the same two people.
     *
     * The rule itself is [me.lakko.app.contacts.conversationWithContact]; the
     * contact card's `Message` and the call screen's `Invite contacts` both
     * ask it here, so they cannot disagree.
     *
     * A linked id whose chat is gone (deleted here, or never present on this
     * device) is NOT an address - seen live on web 2026-09-26 as a contact
     * whose Message opened "Conversation not found". The same trust existed
     * here and is closed the same way.
     */
    public fun chatGroupIdHexFor(linkedGroupIdHex: String?, peerKey: ByteArray): String? =
        me.lakko.app.contacts.conversationWithContact(
            contactKey = outboxB64(peerKey),
            linkedId = linkedGroupIdHex,
            conversations = groups.keys.toList(),
            idOf = { it },
            ownKey = ownPersonKeyB64(),
            // ADR-0050 — [members] is one row per PERSON, keyed on the
            // account key; a roster that cannot be read is nobody.
            peopleOf = { hex -> members(hex)?.map { outboxB64(it.signaturePublicKey) }.orEmpty() },
            isOneToOne = { hex -> groupKind(hex) == GroupKind.ONE_TO_ONE },
            samePerson = { a, b -> a == b || samePersonKey?.invoke(a, b) == true },
        )

    /**
     * One person, one one-to-one row ([duplicateOneToOneFolds], the web
     * client's `duplicate-one-to-one.ts`): a one-to-one the other person
     * founded continues an older one this account founded with them, which
     * their device never opened. Recorded in the conversation lineage
     * (ADR-0073 R.5) through [lineageRecorder], so the list shows one row
     * whose head is the new group and the old journal is no longer polled
     * ([oldSegmentPolled]). Run whenever the held groups or the lineage move;
     * the recorder writes a register this device holds only once.
     */
    public suspend fun foldDuplicateOneToOnes() {
        val recorder = lineageRecorder ?: return
        val own = ownPersonKeyB64() ?: return
        val lineage = lineageNow()
        val records = synchronized(groups) { groups.entries.map { it.key to it.value } }
        val held = records.mapNotNull { (hex, record) ->
            if (hex.lowercase() in endingGroups) return@mapNotNull null
            try {
                val members = record.group.members()
                HeldConversationGroup(
                    groupHex = hex.lowercase(),
                    conversationHex = lineage.conversationOf(hex.lowercase()),
                    isOneToOne = record.group.groupKind() == GroupKind.ONE_TO_ONE,
                    people = personKeysOf(members),
                    founderKey = founderKeyOf(record.group, members),
                )
            } catch (_: Throwable) {
                null
            }
        }
        for (fold in duplicateOneToOneFolds(held, own)) {
            val successor = hexToBytes(fold.successorHex) ?: continue
            val root = hexToBytes(fold.rootHex) ?: continue
            try {
                if (recorder(successor, root)) {
                    android.util.Log.i("ChatRepository", "a one-to-one the other person founded continues an older one")
                }
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "duplicate one-to-one not folded: ${cause.javaClass.simpleName}")
            }
        }
        publishThreadList()
    }

    /** The account key (standard base64) that founded [group]: the role
     *  table's owner, else the person at leaf 0; `null` when unreadable. */
    private fun founderKeyOf(group: Group, members: List<GroupMember>): String? {
        try {
            group.groupRoles()?.owner?.takeIf { it.size == 32 }?.let { return outboxB64(it) }
        } catch (_: Throwable) {
            // fall through to the roster
        }
        return members.firstOrNull { it.leafIndex == 0u }?.let { outboxB64(personKeyOf(it)) }
    }

    /**
     * `docs/ux/who-can-find-me.md` section 6.4 — where a contact request that
     * arrived inside [groupIdHex] goes: the contacts repository opens it with
     * this account's key and runs the person's setting. Installed by it; the
     * hand-over is all that happens here (no network, no lock taken).
     */
    @Volatile
    internal var groupContactRequestIntake:
        ((groupIdHex: String, senderKey: MemberIdentity, envelope: ByteArray, messageId: Long, createdAt: String) -> Unit)? = null

    /** Section 6.4 — write the opened group requests held only in memory, or
     *  throw. Run before a journal cursor moves past their rows, so the row
     *  is either read again or its request already stored. Installed by the
     *  contacts repository. */
    @Volatile
    internal var groupContactRequestsFlush: (() -> Unit)? = null

    private fun receiveGroupContactRequest(
        groupIdHex: String,
        senderKey: MemberIdentity?,
        envelope: ByteArray,
        messageId: Long,
        createdAt: String,
    ) {
        // An unattributable request cannot be checked against its sender.
        if (senderKey == null) return
        groupContactRequestIntake?.invoke(groupIdHex, senderKey, envelope, messageId, createdAt)
    }

    /**
     * Section 6.4, step 2 — the group (not a one-to-one) this account and the
     * person holding [peerKey] are both current members of, the one with the
     * newest message when several; `null` when there is none.
     */
    public fun sharedGroupWith(peerKey: ByteArray): String? {
        val peer = outboxB64(peerKey)
        val own = ownPersonKeyB64() ?: return null
        val lastAt = state.value.threads.associate { it.groupIdHex.lowercase() to threadNewestAtMs(it) }
        return pickSharedGroup(
            candidates = groups.keys.toList(),
            isGroup = { hex -> groupKind(hex) == GroupKind.GROUP && isGroupActive(hex) == true },
            people = { hex -> members(hex)?.map { outboxB64(it.signaturePublicKey) } },
            own = own,
            isPeer = { key -> key == peer || samePersonKey?.invoke(key, peer) == true },
            newestAt = { hex -> lastAt[hex.lowercase()] },
        )
    }

    /** Section 6.4 — post [envelope] (a sealed contact request) into
     *  [groupIdHex]. Returns the DS row id and stamp; throws on failure. */
    public suspend fun sendGroupContactRequest(groupIdHex: String, envelope: ByteArray): SendMessageResult =
        forSession {
            val identity = canonicalIdentity.get()
                ?: throw IllegalStateException("ChatRepository not bootstrapped — call bootstrap() first")
            val record = groups[groupIdHex] ?: throw IllegalStateException("conversation gone")
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = envelope,
                automatic = false,
            ) ?: throw IllegalStateException("the group takes no request")
        }

    /** ADR-0074 section 3.7 — whether two standard-base64 person keys are one
     *  person across proven rotations. Installed by the contacts repository. */
    @Volatile
    internal var samePersonKey: ((String, String) -> Boolean)? = null

    /**
     * The live one-to-one with the person holding [peerKey]: a conversation
     * whose kind is one-to-one (ADR-0062), whose people include that person
     * and whose handle reads active. `null` when there is none. A person
     * removed from the contacts and invited again is put back with it rather
     * than given a second conversation (amended 2026-10-24).
     */
    public fun liveOneToOneWith(peerKey: ByteArray): String? {
        val peer = outboxB64(peerKey)
        for (hex in groups.keys.toList()) {
            if (groupKind(hex) != GroupKind.ONE_TO_ONE) continue
            val people = members(hex) ?: continue
            if (people.none { outboxB64(it.signaturePublicKey) == peer }) continue
            if (isGroupActive(hex) != true) continue
            return hex
        }
        return null
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
        // ADR-0087 UX 9.4 — an admin's own sweep can land the leave before this
        // runs. A person who asked is told nothing, and a record written now
        // would read "Waiting to leave." on a later membership.
        val hex = groupIdHex.lowercase()
        val landed = membershipEnds.load(accountIdB64).firstOrNull { it.idHex == hex }
        dismissEndNotices(listOf(hex))
        if (landed != null) {
            if (landed.stage == EndStage.NOTICE_ONLY) {
                membershipEnds.remove(accountIdB64, hex)
            } else {
                membershipEnds.save(accountIdB64, landed.copy(noticeClosed = true))
            }
            publishEndState()
            return
        }
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
        return forSession {
            withContext(Dispatchers.IO) {
                GroupPushRouting.retireOwnSubscription(
                    accountIdB64 = accountIdB64,
                    groupIdHex = groupIdHex,
                    ports = pushRoutingPorts(groupIdHex, record.group, identity),
                )
            }
        }
    }

    // -----------------------------------------------------------------
    // ADR-0087 section 9 — the end of a membership.
    //
    // Every rule lives in `MembershipEnd.kt`, where the host suite runs it;
    // what is here feeds it live group state and supplies the ports.
    // -----------------------------------------------------------------

    private val membershipEnds: MembershipEndStore by lazy { SharedPrefsMembershipEndStore(applicationContext) }

    private val heldWelcomes: HeldWelcomeStore by lazy { SharedPrefsHeldWelcomeStore(applicationContext) }

    /** The groups an end is running for (lower-case hex): gone from the lists
     *  and from every poll, sweep and beacon. Rows of the durable store, kept
     *  here because the poll path asks on every call. */
    private val endingGroups: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** One run at a time, so two tails never walk one row together. */
    private val membershipEndRun = Mutex()

    private val heldWelcomeRun = java.util.concurrent.atomic.AtomicBoolean(false)

    private val membershipEndListeners =
        java.util.concurrent.CopyOnWriteArrayList<(groupIdHex: String) -> Unit>()

    /**
     * Observe the moment this account is out of a group, before anything of it
     * is deleted: a call in it ends here. Called on the end run's coroutine; a
     * listener only hands the event off. Returns the unsubscribe.
     */
    public fun addMembershipEndListener(listener: (groupIdHex: String) -> Unit): () -> Unit {
        membershipEndListeners += listener
        return { membershipEndListeners -= listener }
    }

    init {
        // After the fields above, and declared before nothing it reads: the
        // collector does nothing until a bootstrap has finished. The register
        // arrives on the sync runtime's own schedule, and a value here is also
        // what an end waiting for an open synced state resumes on.
        pushScope.launch {
            combine(
                mutableState.map { it.bootstrapped }.distinctUntilChanged(),
                ConversationEndsMirror.registers,
            ) { ready, heard -> heard.takeIf { ready } }.collect { heard ->
                if (heard == null) return@collect
                try {
                    pushScope.launchSession { reactToHeardEnds(heard) }
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "ADR-0087: register not acted on: ${cause.javaClass.simpleName}")
                }
            }
        }
    }

    /** The conversation reference a register names a group with. */
    private fun groupRefOf(groupIdHex: String): uniffi.lakko_ffi_uniffi.FolderConversationRef? =
        hexToBytes(groupIdHex)?.let {
            uniffi.lakko_ffi_uniffi.FolderConversationRef(
                kind = uniffi.lakko_ffi_uniffi.FolderConversationKind.GROUP,
                id = it,
            )
        }

    /**
     * Every key this account's leaves may be bound under: the conversation
     * identity, the published one, the proven predecessors. Empty when any of
     * it cannot be read, which reads as "not out": a key left out here is a
     * person's history.
     */
    private fun ownAccountKeysB64(): Set<String> {
        val accountIdB64 = currentAccountIdB64.get() ?: return emptySet()
        return try {
            val keys = LinkedHashSet<String>()
            ownAccountKey()?.let { keys += outboxB64(it) } ?: return emptySet()
            rootAccountIdentity()?.let { keys += outboxB64(it.signaturePublicKey()) }
            val rows = me.lakko.app.auth.OwnPredecessorStore.onDevice(applicationContext).load(accountIdB64)
            try {
                rows.forEach { keys += outboxB64(it.signaturePubkey) }
            } finally {
                rows.forEach { it.zero() }
            }
            keys
        } catch (_: Throwable) {
            emptySet()
        }
    }

    /** Does a leaf of this account stand in [group]'s roster? `null` when it
     *  cannot be told. On the provider critical path. */
    private fun accountInRosterOrNull(group: Group): Boolean? {
        val roster = memberAccountKeysOrNull(group) ?: return null
        val own = ownAccountKeysB64()
        if (own.isEmpty()) return null
        return roster.any { it in own }
    }

    /**
     * The seam: called after a commit was applied, and over the stored groups
     * at launch. Writes the owed record of an end this device has just seen,
     * and nothing else. On the provider critical path (it reads the roster),
     * and it runs no step, because the steps take that lock themselves.
     */
    private fun recordEndIfOutLocked(groupIdHex: String, record: GroupRecord) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val hex = groupIdHex.lowercase()
        if (hex in endingGroups) return
        val isActive = readIsActive { record.group.isActive() }
        if (isActive != false) return
        val out = membershipFullyEnded(
            isActive = isActive,
            isGroupKind = runCatching { record.group.groupKind() }.getOrNull() == GroupKind.GROUP,
            rosterAccountKeys = memberAccountKeysOrNull(record.group),
            ownAccountKeys = ownAccountKeysB64(),
        )
        if (!out) return
        val epoch = runCatching { record.group.epoch() }.getOrNull() ?: return
        val row = appliedEnd(
            idHex = hex,
            name = GroupNameStore.name(applicationContext, accountIdB64, groupIdHex).orEmpty(),
            left = loadLeaveRequestedAt(leaveRecords, accountIdB64, groupIdHex) != null,
            epoch = epoch,
        )
        // Not saved: nothing ran, and the launch sweep sees this state again.
        if (!membershipEnds.save(accountIdB64, row)) return
        noteLiveEndNotice(row)
        endingGroups += hex
        android.util.Log.i("ChatRepository", "ADR-0087: this account is out of a group")
    }

    /** Whether an end of this group is recorded and not yet carried out. */
    public fun isEndOwed(groupIdHex: String): Boolean = groupIdHex.lowercase() in endingGroups

    /** Fill [endingGroups] and the state from the durable rows. A notice is
     *  never stored (UX 9): a row an older build kept only for its notice is
     *  dropped, and no notice is shown at a start. */
    private fun loadOwedEndsBestEffort(accountIdB64: String) {
        val rows = try {
            membershipEnds.load(accountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }
        for (row in rows) if (row.stage == EndStage.NOTICE_ONLY) membershipEnds.remove(accountIdB64, row.idHex)
        endingGroups += endingIdsOf(rows)
        val live = synchronized(liveEndNotices) { liveEndNotices.values.toList() }
        mutableState.update { it.copy(endNotices = live, endingIds = endingGroups.toSet()) }
    }

    private fun noteLiveEndNotice(row: OwedEnd) {
        if (!row.noticeOwed) return
        synchronized(liveEndNotices) {
            liveEndNotices[row.idHex] = MembershipEndNotice(row.idHex, row.name, row.recordedAtMs)
        }
        publishLiveEndNotices()
    }

    private fun publishLiveEndNotices() {
        val list = synchronized(liveEndNotices) { liveEndNotices.values.toList() }
        mutableState.update { it.copy(endNotices = list) }
    }

    /** UX 9 — the bottom notice was shown for [ids] (or the dialog in the
     *  conversation was answered): they are not shown again. */
    public fun dismissEndNotices(ids: Collection<String>) {
        val changed = synchronized(liveEndNotices) {
            ids.map { liveEndNotices.remove(it.lowercase()) != null }.any { it }
        }
        if (changed) publishLiveEndNotices()
    }

    /** Publish what the durable rows say, and the list without what is ending. */
    private fun publishEndState() {
        currentAccountIdB64.get()?.let { loadOwedEndsBestEffort(it) }
        publishThreadList()
    }

    /** The launch sweep: stored groups, owed rows, the register, held Welcomes. */
    private suspend fun sweepMembershipEnds() {
        val records = synchronized(groups) { groups.toMap() }
        providerLock.sessionLock {
            for ((hex, record) in records) recordEndIfOutLocked(hex, record)
        }
        publishEndState()
        ConversationEndsMirror.registers.value?.let { reactToHeardEnds(it) }
        if (endingGroups.isNotEmpty()) runOwedMembershipEnds()
        resolveHeldWelcomes()
    }

    /**
     * Rule 4 of the register: compare epochs before anything stops. Read after
     * every change to the synced state and at launch, and idempotent: a wiped
     * group has nothing left to act on.
     */
    private suspend fun reactToHeardEnds(heard: HeardConversationEnds) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        if (heard.accountIdB64 != accountIdB64) return
        for (end in heard.ends) {
            if (end.channel || end.idHex in endingGroups) continue
            val record = groups[end.idHex]
            val read = record?.let {
                providerLock.sessionLock {
                    readIsActive { it.group.isActive() } to runCatching { it.group.epoch() }.getOrNull()
                }
            }
            when (heardEndVerdict(record != null, read?.first, read?.second, end.epoch)) {
                HeardEndVerdict.NOTHING -> Unit
                HeardEndVerdict.ANSWER_JOINED -> {
                    val ref = groupRefOf(end.idHex) ?: continue
                    val epoch = read?.second ?: continue
                    me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.publishConversationJoined(ref, epoch)
                }
                HeardEndVerdict.ACT -> {
                    val name = GroupNameStore.name(applicationContext, accountIdB64, end.idHex).orEmpty()
                    val row = heardEnd(end.idHex, name, end.epoch)
                    if (membershipEnds.save(accountIdB64, row)) {
                        noteLiveEndNotice(row)
                        endingGroups += end.idHex
                    }
                }
            }
        }
        if (endingGroups.isNotEmpty()) runOwedMembershipEnds()
    }

    /**
     * Walk every owed row. Never under a group or provider lock: the steps take
     * them. A row that stopped on a failed step stays owed, and the next sweep
     * of the groups comes back to it.
     */
    private suspend fun runOwedMembershipEnds() {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        publishEndState()
        membershipEndRun.sessionLock {
            for (row in membershipEnds.load(accountIdB64)) {
                if (row.stage == EndStage.NOTICE_ONLY) {
                    membershipEnds.remove(accountIdB64, row.idHex)
                    continue
                }
                // Recorded, and then the snapshot that holds the commit did not
                // land: the commit comes round again, and the seam with it.
                val record = groups[row.idHex]
                if (row.source == EndSource.APPLIED && record != null &&
                    readIsActive { record.group.isActive() } == true
                ) {
                    membershipEnds.remove(accountIdB64, row.idHex)
                    endingGroups -= row.idHex
                    continue
                }
                val outcome = runMembershipEnd(accountIdB64, row, membershipEnds, membershipEndPorts)
                if (outcome == EndRun.DONE || outcome == EndRun.ABANDONED) endingGroups -= row.idHex
            }
        }
        publishEndState()
        resolveHeldWelcomes()
    }

    private val membershipEndPorts: MembershipEndPorts = object : MembershipEndPorts {
        override suspend fun publishEnded(row: OwedEnd): EndPublish {
            val sync = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active ?: return EndPublish.NOT_OPEN
            val ref = groupRefOf(row.idHex) ?: return EndPublish.NOTHING_TO_TELL
            return sync.publishConversationEnded(ref, row.epochValue)
        }

        override suspend fun stop(row: OwedEnd) {
            for (listener in membershipEndListeners) {
                try {
                    listener(row.idHex)
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "membership end listener failed", cause)
                }
            }
            for (hex in olderSegmentsOf(row.idHex) + row.idHex) {
                val retired = try {
                    groups[hex] != null && retirePushSubscriptionAfterEviction(hex)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (_: Throwable) {
                    false
                }
                if (!retired) keepPushRetireOwed(hex)
            }
        }

        override suspend fun wipeStores(row: OwedEnd) {
            val segments = olderSegmentsOf(row.idHex) + row.idHex
            // A chosen delete has no stop step: whatever the screen's teardown
            // did not retire is kept owed before the route row goes.
            if (row.source == EndSource.CHOSEN) segments.forEach { keepPushRetireOwed(it) }
            for (hex in segments) wipeSegmentStores(hex)
            // A person's own choice on a device whose account may still be in
            // the group says nothing to the account's other devices.
            if (row.source != EndSource.CHOSEN) {
                val sync = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active
                for (hex in segments) groupRefOf(hex)?.let { sync?.clearConversationRegisters(it) }
            }
        }

        override suspend fun deleteMls(row: OwedEnd): MlsDelete {
            val identity = canonicalIdentity.get() ?: return MlsDelete.FAILED
            return groupLock(row.idHex).sessionLock {
                providerLock.sessionLock {
                    for (older in olderSegmentsOf(row.idHex)) {
                        val record = groups[older] ?: continue
                        // A superseded group this device is still in stays in
                        // the provider, inert: only a register may delete a
                        // group that reads active, and none names it.
                        if (readIsActive { record.group.isActive() } == false) {
                            runCatching { record.group.deleteLocally(identity) }
                        }
                    }
                    val record = groups[row.idHex] ?: return@sessionLock MlsDelete.DELETED
                    try {
                        when (row.source) {
                            EndSource.HEARD -> record.group.deleteAfterEndedElsewhere(identity, row.epochValue)
                            EndSource.APPLIED, EndSource.CHOSEN -> record.group.deleteLocally(identity)
                        }
                        MlsDelete.DELETED
                    } catch (_: FfiException.MembershipGroupStillActive) {
                        MlsDelete.STILL_ACTIVE
                    } catch (cause: FfiException) {
                        android.util.Log.w("ChatRepository", "ADR-0087: group not deleted (${cause.javaClass.simpleName})")
                        MlsDelete.FAILED
                    }
                }
            }
        }

        override suspend fun persistDurably(): Boolean = providerLock.sessionLock {
            try {
                persistSnapshotDurableOrThrow()
                true
            } catch (ended: me.lakko.app.StaleBindingException) {
                throw ended
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0087: snapshot not persisted (${cause.javaClass.simpleName})")
                false
            }
        }

        override suspend fun dropRegistry(row: OwedEnd) {
            for (hex in olderSegmentsOf(row.idHex)) dropGroupRecord(hex)
            dropGroupRecord(row.idHex)
        }

        override suspend fun cameBack(row: OwedEnd) {
            val record = groups[row.idHex] ?: return
            val ref = groupRefOf(row.idHex) ?: return
            val epoch = providerLock.sessionLock { runCatching { record.group.epoch() }.getOrNull() } ?: return
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.publishConversationJoined(ref, epoch)
        }
    }

    /**
     * A retire that could not run or land: the route row goes with the wipe, so
     * the subscription id is kept for the next unsubscribe, and a wake that
     * still arrives finds no route and opens nothing (ADR-0078).
     */
    private fun keepPushRetireOwed(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            val secret = PushRoutingSecretStore.get(applicationContext, accountIdB64, groupIdHex) ?: return
            val gidHex = DsClient.pseudonymHex(uniffi.lakko_ffi_uniffi.derivePushGid(secret))
            pushRouteStore.subscriptionId(accountIdB64, gidHex)?.let {
                me.lakko.app.push.PendingPushUnsubscribes(applicationContext).stash(listOf(it))
            }
            pushRouteStore.remove(accountIdB64, gidHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087: push retire not kept: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * The local half of ending one group: every store keyed by it, and the
     * in-memory state, the record's timeline first so a log persist of another
     * group cannot write it back. Idempotent. Profiles are not tied to a group
     * and stay; the synced registers go by their own tombstones.
     */
    private fun wipeSegmentStores(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val hex = groupIdHex.lowercase()
        fun step(what: String, block: () -> Unit) {
            try {
                block()
            } catch (ended: me.lakko.app.StaleBindingException) {
                throw ended
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0087 wipe: $what (${cause.javaClass.simpleName})")
            }
        }
        val record = groups[groupIdHex]
        record?.wipeTimeline()
        synchronized(groups) { waitingLogSlots.take(groupIdHex) }
        step("message log") { MessageLogStore.deleteGroupFromLog(applicationContext, accountIdB64, groupIdHex) }
        // The whole row, own request intents included: the prune keeps those,
        // and they describe a group this device is deleting.
        step("observations") { pendingRequestObservations.save(accountIdB64, groupIdHex, emptyMap()) }
        step("sent envelopes") { sentCommitterEnvelopes.save(accountIdB64, groupIdHex, emptyMap()) }
        step("unapplied rows") { unappliedRequests.clearForGroup(accountIdB64, groupIdHex) }
        step("leave record") { leaveRecords.clear(accountIdB64, groupIdHex) }
        step("pins") { PinStore.deleteGroup(applicationContext, accountIdB64, groupIdHex) }
        step("shared channels") { sharedChannels.forgetGroup(groupIdHex) }
        step("claim state") {
            successorClaims.remove(accountIdB64, hex)
            invalidateDeadSession(groupIdHex)
        }
        step("continuity") { continuityStore.update(accountIdB64) { it.withoutGroup(hex) } }
        step("read watermarks") { readWatermarkStore.deleteGroup(accountIdB64, groupIdHex) }
        step("group call") {
            mutableGroupCalls.update { it - groupIdHex }
            synchronized(groupCallPersistLock) { groupCallStore.save(accountIdB64, groupIdHex, null) }
        }
        step("call log") {
            me.lakko.app.calls.CallHistoryStore.deleteConversation(applicationContext, accountIdB64, conversationIdOf(hex))
            me.lakko.app.calls.CallHistoryStore.deleteConversation(applicationContext, accountIdB64, hex)
        }
        step("own echoes") { if (chatClientLazy.isInitialized()) chatClient.forgetOwnEchoesOf(groupIdHex) }
        step("group name") { GroupNameStore.delete(applicationContext, accountIdB64, groupIdHex) }
        // ADR-0087 section 9 — and its photo, reactions setting, @name and
        // knock seeds, and its unseen reactions.
        step("group settings") { GroupSettingsStore.deleteGroup(applicationContext, accountIdB64, groupIdHex) }
        step("unseen reactions") { UnseenReactions.clear(groupIdHex) }
        step("joined moment") { ConversationJoinedStore.delete(applicationContext, accountIdB64, groupIdHex) }
        step("push secret") { PushRoutingSecretStore.remove(applicationContext, accountIdB64, groupIdHex) }
        step("held") {
            val conversation = conversationIdOf(hex)
            val dropped = listOf(conversation, hex).map { heldSends.remove(it) != null }.any { it }
            if (dropped) persistHeldSends()
            heldCopyRows.remove(groupIdHex)
            heldCopyStore.clear(accountIdB64, groupIdHex)
        }
        step("journal seeds") {
            journalOwnershipStore.remove(accountIdB64, groupIdHex)
            journalOwnershipStore.rootHeld.clear(accountIdB64, groupIdHex)
            journalOwnershipStore.appointments.put(accountIdB64, groupIdHex, emptyMap())
            journalOwnershipStore.registrarList.put(accountIdB64, groupIdHex, emptyMap())
            clearHandoverOwed(accountIdB64, groupIdHex)
        }
        step("commit outbox") {
            hexToBytes(hex)?.let { id ->
                for (entry in CommitOutboxStore.loadForGroup(applicationContext, accountIdB64, outboxB64(id))) {
                    CommitOutboxStore.deleteEntry(applicationContext, entry.id)
                }
            }
        }
        step("beacons") { beaconObservations.save(accountIdB64, groupIdHex, emptyMap()) }
        step("last owner") { lastObservedOwnerStore.clear(accountIdB64, groupIdHex) }
        step("profile requests") { profileRequestStore.deleteGroup(accountIdB64, groupIdHex) }
        step("readmission") { DeviceReadmission.clearGroup(applicationContext, accountIdB64, groupIdHex) }
        // ADR-0091 phase 2 — the behind state, a request record, the knock
        // key held and the gap lines.
        step("reentry") {
            conversationBehind.forget(hex)
            reentryRecords.remove(accountIdB64, hex)
            knockKeys.remove(accountIdB64, hex)
            reentryGapStore.delete(accountIdB64, hex)
            mutableReentryGaps.update { it - hex }
            gapLookStore.remove(accountIdB64, hex)
            owedGapBundleStore.removeGroup(accountIdB64, hex)
        }
        step("reach grants") { me.lakko.app.reach.ReachManager.peek()?.forgetHeldScope(hex) }
        // In memory, per group.
        localUnread.forget(groupIdHex)
        composerCarry.forget(conversationIdOf(hex))
        supersededCheckedEpoch.remove(groupIdHex)
        appointmentAttemptAtMs.remove(groupIdHex)
        lastBeaconAtMs.remove(groupIdHex)
        peerCheckedAt.remove(groupIdHex)
    }

    /** LAST: dropping the record is what makes a group unreachable, and the
     *  known-groups row is what brings it back at the next start. */
    private fun dropGroupRecord(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            knownGroupsStore.remove(accountIdB64, groupIdHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0087: known groups row kept", cause)
        }
        // And from the ids a restored backup named, or the next upload would
        // list a conversation the person no longer has.
        dropRestoredKnownGroupBestEffort(groupIdHex)
        val removed = synchronized(groups) { groups.remove(groupIdHex) }
        dsSigners.remove(dsGroupKeyHex(groupIdHex))
        try {
            removed?.group?.close()
        } catch (_: Throwable) {
            // best-effort — releasing the Rust-side Arc must not fail a delete.
        }
        // Again, now that no record can write it: a log persist that read the
        // records before the timeline went may have landed since.
        try {
            MessageLogStore.deleteGroupFromLog(applicationContext, accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            // best-effort — a join refuses a slot of a group the register ended
        }
    }

    /**
     * What a join owes the membership it replaced, and the register. [fresh]:
     * the person was out, so what is left of that membership goes before any
     * row of the new one is read. Any row owed for the group goes, its notice
     * too: the sentence is no longer true. Failures here never fail the join.
     */
    private suspend fun settleJoinedOverEnd(
        groupIdHex: String,
        identity: Identity,
        fresh: Boolean,
        group: Group?,
        epoch: ULong?,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val hex = groupIdHex.lowercase()
        val ref = groupRefOf(groupIdHex)
        val sync = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active
        if (fresh) {
            try {
                retirePushSubscriptionAfterEviction(groupIdHex)
                wipeSegmentStores(groupIdHex)
                if (ref != null) sync?.clearConversationRegisters(ref)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "ADR-0087: join over an end: ${cause.javaClass.simpleName}")
            }
            if (group != null) providerLock.sessionLock { ensurePushRoutingBestEffort(group, identity) }
        }
        val owed = hex in endingGroups || membershipEnds.load(accountIdB64).any { it.idHex == hex }
        if (owed) {
            membershipEnds.remove(accountIdB64, hex)
            endingGroups -= hex
            publishEndState()
        }
        // A first join finds no register and writes none. Launched: the join
        // of a person back into a group does not wait for the DeviceGroup.
        if (ref != null && epoch != null && sync != null) {
            pushScope.launchSession { sync.publishConversationJoined(ref, epoch) }
        }
    }

    /** The notice was closed (the dialog's OK): the name goes with it. */
    public fun dismissEndNotice(idHex: String) {
        dismissEndNotices(listOf(idHex))
    }

    /** A channel went (ADR-0087 section 9.5): the channel layer dropped it,
     *  and the person is told like for a group, live and never stored. */
    public fun noteChannelEndHeard(channelIdHex: String, name: String) {
        if (currentAccountIdB64.get() == null) return
        noteLiveEndNotice(heardEnd(channelIdHex, name, 0u))
    }

    /**
     * A Welcome the core refused because this device holds the group as active.
     * Kept durably before the mailbox cursor moves past it; `false` when it did
     * not reach the disk, and the caller then keeps the cursor.
     */
    public fun holdWelcome(groupIdHex: String, welcomeBytes: ByteArray): Boolean {
        val accountIdB64 = currentAccountIdB64.get() ?: return false
        val saved = heldWelcomes.add(accountIdB64, HeldWelcome(groupIdHex.lowercase(), outboxB64(welcomeBytes)))
        if (saved) pushScope.launchSession { resolveHeldWelcomes() }
        return saved
    }

    /** Read [groupIdHex]'s journal to its end. `false` when a read did not
     *  answer: nothing may be concluded from a read that failed. */
    private suspend fun journalDrained(groupIdHex: String): Boolean {
        repeat(MAX_HELD_WELCOME_PAGES) {
            val record = groups[groupIdHex] ?: return true
            val before = record.cursor
            val outcome = try {
                pollGroup(groupIdHex, waitSecs = 0, joinInFlight = true)
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                return false
            }
            when (outcome) {
                GroupPollOutcome.APPLIED -> if (groups[groupIdHex]?.cursor == before) return true
                GroupPollOutcome.UNAVAILABLE -> return true
                else -> delay(HELD_WELCOME_RETRY_DELAY_MS)
            }
        }
        return false
    }

    /**
     * Each held Welcome: read its group's journal and retry the join once the
     * group ended, drop it when the journal is drained and the group is still
     * active (a duplicate or a forgery: the core does not report a Welcome's
     * epoch, so no client can tell sooner), keep it when a read failed.
     */
    private suspend fun resolveHeldWelcomes() {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        if (heldWelcomes.load(accountIdB64).isEmpty()) return
        if (!heldWelcomeRun.compareAndSet(false, true)) return
        try {
            for (held in heldWelcomes.load(accountIdB64)) {
                val hex = held.groupIdHex
                // An end still running decides this on a later pass.
                if (hex in endingGroups) continue
                val drained = journalDrained(hex)
                if (hex in endingGroups) continue
                val reentryRecorded = recordedReentryKeyPackages(hex).isNotEmpty()
                when (heldWelcomeVerdict(drained, isGroupActive(hex), reentryRecorded)) {
                    HeldWelcomeVerdict.KEEP -> Unit
                    HeldWelcomeVerdict.DROP -> {
                        android.util.Log.i("ChatRepository", "a held Welcome was dropped: the group is still active")
                        heldWelcomes.remove(accountIdB64, held)
                    }
                    HeldWelcomeVerdict.RETRY -> try {
                        joinGroupFromWelcome(outboxUnb64(held.welcomeB64))
                        heldWelcomes.remove(accountIdB64, held)
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (cause: Throwable) {
                        // Still refused: the next pass reads the journal again.
                        if (groupStillActiveHex(cause) == null) {
                            android.util.Log.w("ChatRepository", "a held Welcome failed", cause)
                            heldWelcomes.remove(accountIdB64, held)
                        }
                    }
                }
            }
        } finally {
            heldWelcomeRun.set(false)
        }
    }

    // -----------------------------------------------------------------
    // ADR-0091 phase 2 — a device that fell behind comes back
    // -----------------------------------------------------------------

    /** Per conversation, whether this device is behind (section 1). */
    internal val conversationBehind: ConversationBehindTracker by lazy {
        ConversationBehindTracker(
            store = SharedPrefsConversationBehindStore(applicationContext),
            refusedReadsToSuspect = {
                try {
                    uniffi.lakko_ffi_uniffi.reentryRefusedReadsToSuspect().toInt()
                } catch (_: Throwable) {
                    REENTRY_REFUSED_READS_DEFAULT
                }
            },
        ).also { tracker ->
            // Walk 3, W3-R5 — back in step: this device re-sends its own
            // growth-time state for the conversation, as on a growth.
            tracker.onBackInStep = { hex -> pushScope.launchSession { resendOwnGrowthStateBestEffort(hex) } }
        }
    }

    /** The requests this device sent, durably, before each send (section 3). */
    private val reentryRecords: ReentryRecordStore by lazy { SharedPrefsReentryRecordStore(applicationContext) }

    /** The groups' knock keys members hold (route 3). */
    private val knockKeys: KnockKeyStore by lazy { SharedPrefsKnockKeyStore(applicationContext) }

    /** The gap lines (UX 2.4). */
    private val reentryGapStore: ReentryGapStore by lazy { SharedPrefsReentryGapStore(applicationContext) }

    /** The re-admitter's per-leaf facts (section 4). In memory. */
    private val reentryLedger = ReentryAdmissionLedger()

    /** The core's rules, replaceable in tests. */
    internal var reentryRequestDue: ReentryRequestDue = NativeReentryRequestDue
    internal var reentryAdmissionRule: ReentryAdmissionRule = NativeReentryAdmissionRule

    /** Gap bundles this device owes as a re-admitter (section 7), by group
     *  and returning leaf. Durable: a restart before the replacement merges
     *  still sends it. */
    private val owedGapBundleStore: OwedGapBundleStore by lazy { SharedPrefsOwedGapBundleStore(applicationContext) }

    /** The returning device's looks for its gap bundle (section 7): sealed
     *  at rest, the secret included. */
    private val gapLookStore: GapLookStore by lazy { EncryptedGapLookStore(applicationContext) }

    private val gapHistoryLooker: GapHistoryLooker by lazy { GapHistoryLooker(gapLookStore) }

    /** Requests being sent, by group: one at a time per group. */
    private val reentryRequestsInFlight: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** What the owner last sent its knock key with, per group, after a
     *  successful broadcast only (walk 2, R4). In memory: a restart sends it
     *  once more, which a member drops as not newer. */
    private val knockKeySentMarks = java.util.concurrent.ConcurrentHashMap<String, KnockKeySentMark>()

    private val mutableReentryGaps = MutableStateFlow<Map<String, List<ReentryGap>>>(emptyMap())

    /** The gap lines per conversation, for the timeline (UX 2.4). */
    internal val reentryGaps: StateFlow<Map<String, List<ReentryGap>>> = mutableReentryGaps.asStateFlow()

    /** Load [groupIdHex]'s gap lines into [reentryGaps] (the chat opened). */
    internal fun loadReentryGaps(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val hex = groupIdHex.lowercase()
        val gaps = try {
            reentryGapStore.load(accountIdB64, hex)
        } catch (_: Throwable) {
            emptyList()
        }
        mutableReentryGaps.update { if (gaps.isEmpty()) it - hex else it + (hex to gaps) }
    }

    private fun saveReentryGaps(groupIdHex: String, gaps: List<ReentryGap>) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val hex = groupIdHex.lowercase()
        try {
            if (!reentryGapStore.save(accountIdB64, hex, gaps)) {
                android.util.Log.w(REENTRY_LOG_TAG, "gap line not saved")
            }
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "gap line not saved (${cause.javaClass.simpleName})")
        }
        mutableReentryGaps.update { if (gaps.isEmpty()) it - hex else it + (hex to gaps) }
    }

    /** Who can bring this device back in [groupIdHex], for the waiting note:
     *  the route its last request took, `null` before one was sent. Reads
     *  no group state (safe on the main thread). */
    internal fun reentryHelper(groupIdHex: String): ReentryHelper? {
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        return try {
            reentryRecords.get(accountIdB64, groupIdHex)?.requests?.lastOrNull()?.let { helperOf(it.route) }
        } catch (_: Throwable) {
            null
        }
    }

    /** Walk 3, W3-R2 — an automatic send was held ([broadcastControlToGroup]
     *  answered `null`): for callers that count only a thrown send as not
     *  done. Their catch blocks are silent, so a held send stays quiet. */
    private class AutomaticSendHeld : Exception("automatic send held")

    /**
     * Walk 3, W3-R5 — a person's own action that is not a message (an edit,
     * a deletion, a reaction, a poll or checklist or article op, a shared pin,
     * a rename, a group photo, the reactions setting, a group handle, a
     * contact request inside a group) is refused while this device is
     * suspected or confirmed behind in [groupIdHex]. A refused read alone
     * does not hold it. The error carries no text of its own, so the caller
     * shows the action's ordinary failure message.
     */
    internal fun requirePersonActionAllowed(groupIdHex: String) {
        if (behindStopsActing(groupIdHex)) throw personActionHeldError()
    }

    /** ADR-0091 section 1 — a `401` on a publish into [groupIdHex] (clock not
     *  skewed) is evidence, whatever was being sent. Rethrows. */
    private inline fun <T> withPublishEvidence(groupIdHex: String, block: () -> T): T =
        try {
            block()
        } catch (cause: Throwable) {
            if (proofRefusedWithClockOk(cause)) conversationBehind.notePublishRefused(groupIdHex)
            throw cause
        }

    /** W3-R5 — a person's own op on a message: refused while behind (a dead
     *  one-to-one keeps its local-only path), and a refused publish is evidence. */
    private suspend fun <T> personAction(groupIdHex: String, block: suspend () -> T): T {
        if (!deadOneToOne(groupIdHex)) requirePersonActionAllowed(groupIdHex)
        return withPublishEvidence(groupIdHex) { block() }
    }

    /** Walk 3, W3-R2 — `true` while automatic sends into [groupIdHex] are held. */
    private fun behindHoldsAutomaticSends(groupIdHex: String): Boolean =
        try {
            conversationBehind.holdsAutomaticSends(groupIdHex)
        } catch (_: Throwable) {
            false
        }

    /** ADR-0091 — `true` while this device stops acting in [groupIdHex]. */
    private fun behindStopsActing(groupIdHex: String): Boolean =
        try {
            conversationBehind.stopsActing(groupIdHex)
        } catch (_: Throwable) {
            false
        }

    /** A `401` proof refusal whose clock was not skewed, anywhere in [error]'s causes. */
    private fun proofRefusedWithClockOk(error: Throwable?): Boolean {
        var current = error
        var depth = 0
        while (current != null && depth < 8) {
            if (current is me.lakko.app.ds.DsProofRefusedError) return !current.clockSkewed
            current = current.cause
            depth += 1
        }
        return false
    }

    /**
     * An UN-ACKED ADR-0040 outbox entry or a staged sweep awaits a verdict.
     * Provider lock. An acked entry owes nothing (walk 2: an acked sweep entry
     * left on disk kept a device "owing" forever, so refused reads never
     * suspected it); the rule is [ownCommitOwedBy].
     */
    private fun ownCommitOwedLocked(record: GroupRecord): Boolean {
        val staged = runCatching { record.group.hasStagedSweep() }.getOrDefault(true)
        if (staged) return true
        val accountIdB64 = currentAccountIdB64.get() ?: return true
        return try {
            ownCommitOwedBy(
                stagedSweep = false,
                entries = CommitOutboxStore.loadForGroup(applicationContext, accountIdB64, outboxB64(record.groupId)),
            )
        } catch (_: Throwable) {
            true
        }
    }

    /**
     * Section 1 — fold what one read of [groupIdHex] showed. Inside the
     * poll's locked apply region (the provider is read), after the cursor gate.
     */
    private fun observeBehindLocked(
        groupIdHex: String,
        record: GroupRecord,
        raw: me.lakko.app.ds.FetchResult,
        result: PollMessagesResult,
    ) {
        try {
            for (row in result.skipped) {
                conversationBehind.noteSkipped(
                    groupIdHex,
                    row.sealedEpoch,
                    row.localEpoch,
                    me.lakko.app.ui.chat.parseMessageTimeMs(row.createdAt),
                )
            }
            if (result.lastRowApplied) conversationBehind.noteApplied(groupIdHex)
            // Walk 2, R1 — the newest row this device applied: where a gap
            // would start, known before any request goes out.
            conversationBehind.noteRowKnown(groupIdHex, result.lastAppliedAtMs)
            if (readIsActive { record.group.isActive() } == false) return
            val epoch = record.group.epoch()
            // Section 4 — the re-admitter's "the leaf is writing" fact: the
            // application rows of a batch with no commit decrypted at this
            // epoch, so their leaves are resolved against this roster.
            val commitInBatch = result.events.any { it is IncomingChatEvent.System && it.systemKind == SystemKind.Commit }
            if (!commitInBatch) {
                val roster = rosterOf(record.group)
                for (event in result.events) {
                    if (event !is IncomingChatEvent.Application) continue
                    val leaf = event.senderLeaf ?: continue
                    val key = roster.firstOrNull { it.leafIndex == leaf }?.signaturePublicKey ?: continue
                    reentryLedger.noteMessage(
                        groupIdHex,
                        me.lakko.app.ds.hexOf(key),
                        epoch,
                        me.lakko.app.ui.chat.parseMessageTimeMs(event.createdAt) ?: System.currentTimeMillis(),
                    )
                }
            }
            conversationBehind.observe(
                groupIdHex = groupIdHex,
                ownCommitOwed = ownCommitOwedLocked(record),
                drained = raw.messages.isEmpty() && !raw.hasMore && !result.held,
                latestEpoch = raw.latestEpoch,
                localEpoch = epoch,
            )
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "behind check failed (${cause.javaClass.simpleName})")
        }
    }

    /**
     * Section 1 — a signed read of [groupIdHex] was refused. Outside the locks.
     * A device this suspects asks to be brought back from here too (walk 2):
     * with every read refused no poll applies, so the poll tail that sends
     * the request otherwise never runs. Launched, so the request (which takes
     * the provider lock and goes to the network) never runs inside the
     * poll's in-flight latch.
     */
    private suspend fun noteReadRefusedBestEffort(groupIdHex: String, record: GroupRecord, error: Throwable) {
        if (!proofRefusedWithClockOk(error)) return
        try {
            val (epoch, owed) = providerLock.sessionLock {
                (runCatching { record.group.epoch() }.getOrNull() ?: return@sessionLock null) to ownCommitOwedLocked(record)
            } ?: return
            val state = conversationBehind.noteReadRefused(groupIdHex, owed, epoch)
            if (reentryRequestAfterRefusedRead(state)) {
                pushScope.launchSession { requestReentryIfDueBestEffort(groupIdHex) }
            }
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            // bookkeeping only
        }
    }

    /**
     * Section 3 — the route a request from this device takes in [record]'s
     * group and, for a one-to-one, the other person's account key. Provider
     * lock (or a read the caller tolerates failing).
     */
    private fun reentryRouteLocked(record: GroupRecord, identity: Identity): Pair<ReentryRouteKind, ByteArray?>? {
        val roster = rosterOf(record.group)
        if (roster.isEmpty()) return null
        val ownLeaf = identity.signaturePublicKey()
        val ownPerson = ownIdentity(record.group) ?: return null
        val ownDeviceInConversation = roster.any {
            outboxB64(personKeyOf(it)) == ownPerson && !it.signaturePublicKey.contentEquals(ownLeaf)
        }
        val kind = record.group.groupKind() ?: GroupKind.GROUP
        val route = when (uniffi.lakko_ffi_uniffi.reentryRoute(ownDeviceInConversation, kind)) {
            uniffi.lakko_ffi_uniffi.FfiReentryRoute.OWN_DEVICE -> ReentryRouteKind.OWN_DEVICE
            uniffi.lakko_ffi_uniffi.FfiReentryRoute.MAILBOX -> ReentryRouteKind.MAILBOX
            uniffi.lakko_ffi_uniffi.FfiReentryRoute.KNOCK -> ReentryRouteKind.KNOCK
        }
        val other = roster.map { personKeyOf(it) }.firstOrNull { outboxB64(it) != ownPerson }
        return route to other
    }

    /** Wait this long before trying again after a request that was not sent. Chosen, not measured. */
    private val reentryRetryAfterFailureMs: Long = 120_000L

    /**
     * Section 3 — while this device is behind in [groupIdHex], ask to be
     * brought back when the core says a request is due: mint a re-entry
     * KeyPackage, record it durably, then send it by the route that applies.
     * Best effort; a failed send is tried again after a short wait.
     */
    private suspend fun requestReentryIfDueBestEffort(groupIdHex: String) {
        val hex = groupIdHex.lowercase()
        if (!reentryRequestsInFlight.add(hex)) return
        try {
            requestReentryIfDue(groupIdHex)
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "re-entry request failed (${errorLogTag(cause)})")
        } finally {
            reentryRequestsInFlight.remove(hex)
        }
    }

    private suspend fun requestReentryIfDue(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        val entry = conversationBehind.entry(groupIdHex)
        val now = System.currentTimeMillis()
        if (!reentryRequestDue.due(entry.state, entry.lastRequestSentMs, now)) return
        val (route, otherAccount) = providerLock.sessionLock { reentryRouteLocked(record, identity) } ?: return
        // What the route needs, before anything is minted.
        val knock: HeldKnockKey? = if (route == ReentryRouteKind.KNOCK) {
            knockKeys.get(accountIdB64, groupIdHex)
                ?: providerLock.sessionLock { groupKnockKeyFor(record, groupIdHex) }
                    ?.let { (pk, v) -> HeldKnockKey(v.toInt(), outboxB64(pk)) }
                ?: run {
                    android.util.Log.i(REENTRY_LOG_TAG, "re-entry request waits: no knock key known (route=knock)")
                    conversationBehind.noteRequestSent(groupIdHex, now - reentryIntervalMs() + reentryRetryAfterFailureMs)
                    return
                }
        } else {
            null
        }
        if (route == ReentryRouteKind.MAILBOX && otherAccount == null) return
        if (route == ReentryRouteKind.OWN_DEVICE &&
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.active == null
        ) {
            return
        }
        // Walk 3, W3-R1 — every request (the first and every resend) learns
        // the journal's epoch from an empty knock made right here, after the
        // route is resolved and right before minting (one `ds-knock` token;
        // the knock stores nothing). Never a stored latest epoch: a value kept
        // from an earlier read goes stale while reads are refused, and the
        // re-admitter does not act on a request naming another epoch. A
        // probe that fails or names none sends nothing; tried again shortly.
        val epochLeft = probeReentryEpochLeft { dsClient.knock(record.groupId, emptyList()).epoch } ?: run {
            android.util.Log.i(REENTRY_LOG_TAG, "re-entry request waits: the journal epoch probe failed")
            conversationBehind.noteRequestSent(groupIdHex, now - reentryIntervalMs() + reentryRetryAfterFailureMs)
            return
        }
        // Walk 2, R1 — the first skipped row, else the newest row known or
        // the last good read, never the moment of suspicion when either is
        // known (with every read refused no row is ever skipped).
        val gapFromMs = conversationBehind.gapStartMs(groupIdHex)
        // Mint K under this device's own identity; its private keys are the
        // only thing that opens the Welcome, so they are durable first.
        val keyPackage = providerLock.sessionLock {
            val kp = identity.createReentryKeyPackage(record.groupId, epochLeft)
            persistSnapshotDurableOrThrow()
            kp
        }
        val row = ReentryRequestRow(
            keyPackageB64 = outboxB64(keyPackage),
            epochLeft = epochLeft.toLong(),
            gapFromMs = gapFromMs,
            sentAtMs = now,
            route = route,
        )
        val kept = try {
            uniffi.lakko_ffi_uniffi.reentryRequestsKept().toInt()
        } catch (_: Throwable) {
            REENTRY_REQUESTS_KEPT_DEFAULT
        }
        val (next, dropped) = withReentryRequest(reentryRecords.get(accountIdB64, groupIdHex), groupIdHex, row, kept)
        // Durable BEFORE the send (section 3): a request whose record did not
        // land is not sent.
        if (!reentryRecords.put(accountIdB64, next)) {
            android.util.Log.w(REENTRY_LOG_TAG, "re-entry record not saved; nothing sent")
            return
        }
        if (dropped.isNotEmpty()) {
            providerLock.sessionLock {
                for (old in dropped) {
                    runCatching { identity.forgetKeyPackage(outboxUnb64(old.keyPackageB64)) }
                }
                persistSnapshotBestEffort()
            }
        }
        val envelope = uniffi.lakko_ffi_uniffi.composeConversationReentryEnvelope(
            record.groupId,
            epochLeft,
            gapFromMs.coerceAtLeast(0L).toULong(),
            keyPackage,
        )
        val sent = try {
            when (route) {
                ReentryRouteKind.OWN_DEVICE ->
                    me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.sendConversationReentry(envelope) != null
                ReentryRouteKind.MAILBOX -> sendReentryToMailbox(otherAccount!!, envelope)
                ReentryRouteKind.KNOCK -> {
                    val held = knock!!
                    val sealed = uniffi.lakko_ffi_uniffi.sealReentryKnockRow(
                        outboxUnb64(held.publicKeyB64),
                        held.version.toUShort(),
                        envelope,
                    )
                    dsClient.knock(record.groupId, listOf(sealed))
                    true
                }
            }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "re-entry request not sent: route=${route.name.lowercase()} (${errorLogTag(cause)})")
            false
        }
        if (sent) {
            conversationBehind.noteRequestSent(groupIdHex, now)
            android.util.Log.i(REENTRY_LOG_TAG, "re-entry request sent: route=${route.name.lowercase()} epoch_left=$epochLeft")
        } else {
            conversationBehind.noteRequestSent(groupIdHex, now - reentryIntervalMs() + reentryRetryAfterFailureMs)
        }
    }

    private fun reentryIntervalMs(): Long =
        try {
            uniffi.lakko_ffi_uniffi.reentryRequestIntervalMs().toLong()
        } catch (_: Throwable) {
            30L * 60L * 1_000L
        }

    /** Route 2 — seal the request to the other person's mailbox with each
     *  grant this device holds for them, in turn. `true` once written. */
    private suspend fun sendReentryToMailbox(otherAccount: ByteArray, envelope: ByteArray): Boolean {
        val candidates = me.lakko.app.reach.ReachManager.get(applicationContext)
            .writeDestCandidates(otherAccount, me.lakko.app.reach.WritePurpose.WELCOME)
        for (candidate in candidates) {
            val dest = try {
                candidate()
            } catch (_: Throwable) {
                null
            } ?: continue
            val sealed = mailboxCrypto.sealFor(dest, me.lakko.app.contacts.MailboxRowKind.CONVERSATION_REENTRY, envelope)
            try {
                dsClient.publishMailbox(sealed.address, sealed.row, reach = dest.auth)
                return true
            } catch (refused: me.lakko.app.ds.DsReachRefusedError) {
                continue
            }
        }
        return false
    }

    /**
     * Section 4 — a request to bring a device back reached this device (route
     * 1 from the DeviceGroup, 2 from the account mailbox, 3 from a knock
     * queue). Launched; every check runs on the request's own merits.
     */
    internal fun receiveConversationReentry(envelope: ByteArray, route: ReentryRouteKind) {
        val decoded = try {
            uniffi.lakko_ffi_uniffi.decodeConversationReentryEnvelope(envelope)
        } catch (_: Throwable) {
            android.util.Log.i(REENTRY_LOG_TAG, "re-entry request dropped: malformed")
            return
        }
        pushScope.launchSession { handleReentryRequestBestEffort(decoded, route) }
    }

    private suspend fun handleReentryRequestBestEffort(
        request: uniffi.lakko_ffi_uniffi.FfiConversationReentry,
        route: ReentryRouteKind,
    ) {
        try {
            handleReentryRequest(request, route)
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "re-entry request not handled (${errorLogTag(cause)})")
        }
    }

    private suspend fun handleReentryRequest(
        request: uniffi.lakko_ffi_uniffi.FfiConversationReentry,
        route: ReentryRouteKind,
    ) {
        val identity = canonicalIdentity.get() ?: return
        val groupIdHex = DsClient.pseudonymHex(request.groupId)
        val record = groups[groupIdHex]
        val localEpoch = record?.let { providerLock.sessionLock { runCatching { it.group.epoch() }.getOrNull() } }
        val skip = ReentryRequestCheck.check(
            held = record != null && isGroupActive(groupIdHex) != false,
            selfBehind = behindStopsActing(groupIdHex),
            localEpoch = localEpoch,
            epochLeft = request.epochLeft,
        )
        if (skip != null || record == null || localEpoch == null) {
            android.util.Log.i(REENTRY_LOG_TAG, "re-entry request not acted on: ${skip?.name?.lowercase()}")
            return
        }
        val target = providerLock.sessionLock {
            try {
                record.group.reentryRequestTarget(identity, request.keyPackage)
            } catch (_: Throwable) {
                null
            }
        } ?: run {
            android.util.Log.i(REENTRY_LOG_TAG, "re-entry request refused by the core checks")
            return
        }
        val leafHex = me.lakko.app.ds.hexOf(target.signatureKey)
        // Section 4 — the device is still the account's: route 1 a device of
        // THIS account in its own DeviceGroup roster; routes 2 and 3 a device
        // the account's directory entry lists.
        val stillTheAccounts = when (route) {
            ReentryRouteKind.OWN_DEVICE ->
                outboxB64(target.accountKey) in ownAccountKeysB64() &&
                    ownDeviceRoster()?.any { it.equals(leafHex, ignoreCase = true) } == true
            ReentryRouteKind.MAILBOX, ReentryRouteKind.KNOCK ->
                directoryListsDevice(directoryDeviceKeysOf(target.accountKey), leafHex)
        }
        val now = System.currentTimeMillis()
        val facts = reentryLedger.facts(groupIdHex, leafHex, localEpoch, stillTheAccounts)
        val verdict = reentryAdmissionRule.verdict(facts, now)
        reentryLedger.noteHandled(groupIdHex, leafHex, localEpoch)
        if (verdict != ReentryAdmission.ADMIT) {
            android.util.Log.i(REENTRY_LOG_TAG, "re-entry request not admitted: ${verdict.name.lowercase()}")
            return
        }
        // The replacement: Add(K) then Remove(L), both proposed by this
        // device (the core refuses a Remove standing on the pair before its
        // Add is stored), through the ADR-0045 path.
        val published = groupLock(groupIdHex).sessionLock {
            val proposals = providerLock.sessionLock {
                if (runCatching { record.group.epoch() }.getOrNull() != request.epochLeft) return@sessionLock null
                val p = record.group.proposeReentryReplacement(identity, request.keyPackage)
                persistProposalOrThrow()
                p
            } ?: return@sessionLock false
            dsClient.publishGroup(record.groupId, proposals.add)
            dsClient.publishGroup(record.groupId, proposals.remove)
            true
        }
        if (!published) return
        reentryLedger.noteAccepted(groupIdHex, leafHex, now)
        android.util.Log.i(REENTRY_LOG_TAG, "re-entry admitted: route=${route.name.lowercase()} epoch=$localEpoch")
        // Section 7 — the messages of the gap, once the replacement lands.
        // Durable: the replacement may merge after a restart.
        currentAccountIdB64.get()?.let { accountIdB64 ->
            val written = try {
                owedGapBundleStore.put(
                    accountIdB64,
                    owedGapBundleKey(groupIdHex, leafHex),
                    OwedGapBundle(
                        groupIdHex = groupIdHex.lowercase(),
                        leafSignatureKey = target.signatureKey,
                        epochLeft = request.epochLeft,
                        gapFromMs = request.gapFromMs.toLong(),
                        owedSinceMs = now,
                    ),
                )
            } catch (ended: me.lakko.app.StaleBindingException) {
                throw ended
            } catch (_: Throwable) {
                false
            }
            if (!written) android.util.Log.w(REENTRY_LOG_TAG, "owed gap bundle not saved")
        }
        try {
            sweepIfCommitter(groupIdHex)
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            // the next poll tail sweeps
        }
        sendOwedGapBundlesBestEffort(groupIdHex)
    }

    /**
     * The device keys (hex) [accountKey]'s directory entry lists, or `null`
     * when this device cannot name the account or the directory did not
     * answer (section 4, routes 2 and 3: fail closed).
     */
    private suspend fun directoryDeviceKeysOf(accountKey: ByteArray): List<String>? {
        val account = keyPackageAccount.get() ?: return null
        val keyB64 = outboxB64(accountKey)
        val accountId: ByteArray = me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext, account.accountId)
            .firstOrNull { it.request.fromPubkey.contentEquals(accountKey) }
            ?.request?.fromAccountId
            ?: me.lakko.app.profile.ProfileDirectory.get(applicationContext).profiles.get(keyB64)
                ?.accountIdHex?.let { hexToBytes(it) }
            ?: return null
        return try {
            discovery.fetchKeyPackages(accountId)?.map { me.lakko.app.ds.hexOf(it.deviceId) }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * Section 7 — send each gap bundle this device owes for [groupIdHex] once
     * its group stands at the epoch the returning device joins at: the rows
     * this device holds from the request's gap start on, sealed under an
     * exporter secret of that epoch and posted to a rendezvous only the
     * group and the device can name.
     */
    private suspend fun sendOwedGapBundlesBestEffort(groupIdHex: String) {
        val hex = groupIdHex.lowercase()
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val owed = try {
            owedGapBundleStore.load(accountIdB64).filter { it.value.groupIdHex == hex }
        } catch (_: Throwable) {
            emptyMap()
        }
        if (owed.isEmpty()) return
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        for ((key, bundle) in owed) {
            try {
                val epoch = providerLock.sessionLock { runCatching { record.group.epoch() }.getOrNull() }
                when (owedBundleStep(bundle, epoch, System.currentTimeMillis())) {
                    OwedBundleStep.WAIT -> continue
                    OwedBundleStep.DROP -> {
                        if (owedGapBundleStore.take(accountIdB64, key) != null) {
                            android.util.Log.i(REENTRY_LOG_TAG, "gap bundle dropped: the join epoch passed or it expired")
                        }
                    }
                    OwedBundleStep.SEND -> {
                        // The one claim: another poll path that reached the
                        // same row finds it gone and sends nothing.
                        owedGapBundleStore.take(accountIdB64, key) ?: continue
                        // The replacement landed: what a newcomer is handed on
                        // growth goes to the returning device at once (it keeps
                        // its signature key, so no growth is seen anywhere).
                        resendGrowthStateAfterReentry(groupIdHex)
                        val (secret, ownKey) = providerLock.sessionLock {
                            record.group.exportSecret(identity, REENTRY_HISTORY_EXPORTER_LABEL, bundle.leafSignatureKey, 32u) to
                                ownIdentity(record.group)
                        }
                        persistMessageLogBestEffort()
                        val rendezvous = reentryHistoryRendezvous(secret)
                        val sealed = try {
                            me.lakko.app.devices.HistoryBundleCodec.buildGroupSince(
                                applicationContext,
                                accountIdB64,
                                secret,
                                hex,
                                bundle.gapFromMs,
                                // Walk 3, W3-R3 — an outgoing row names this
                                // account as its author.
                                ownAuthorKeyB64 = ownKey,
                            )
                        } finally {
                            secret.fill(0)
                        }
                        if (sealed == null) {
                            android.util.Log.i(REENTRY_LOG_TAG, "gap bundle empty: nothing held in the gap")
                            continue
                        }
                        // The pairing flow's history-bundle path (a rendezvous
                        // post), at the id both ends derive from the secret.
                        me.lakko.app.devices.WelcomeCourier.publishHistoryBundle(
                            rendezvousPseudonym = rendezvous,
                            sealedBytes = sealed,
                        )
                        android.util.Log.i(REENTRY_LOG_TAG, "gap bundle sent")
                    }
                }
            } catch (ended: me.lakko.app.StaleBindingException) {
                throw ended
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w(REENTRY_LOG_TAG, "gap bundle not sent (${errorLogTag(cause)})")
            }
        }
    }

    /**
     * Section 3 route 3 — the owner hands every member the group's knock key
     * in-group, on a version it has not sent and while the group holds a
     * member it was not sent to (walk 2, R4: the members are compared, not a
     * "grew" flag of one poll, which the owner's own sweep never raised).
     * Best effort; a skipped or failed broadcast leaves it due.
     */
    private suspend fun distributeKnockKeyIfDueBestEffort(groupIdHex: String, afterReentry: Boolean = false) {
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        val hex = groupIdHex.lowercase()
        try {
            val key = providerLock.sessionLock {
                val own = ownAccountKey()
                val table = roleTableOrNull(record.group)
                val isAdmin = own != null && table?.admins?.any { it.contentEquals(own) } == true
                if (!knockKeySender(ownsGroupAsOwner(record.group), isAdmin, afterReentry)) return@sessionLock null
                if (record.group.groupKind() == GroupKind.ONE_TO_ONE) return@sessionLock null
                val members = rosterOf(record.group).map { me.lakko.app.ds.hexOf(it.signaturePublicKey) }.toSet()
                if (members.isEmpty()) return@sessionLock null
                groupKnockKeyFor(record, groupIdHex)?.let { Triple(it.first, it.second, members) }
            } ?: return
            val (pk, version, members) = key
            if (!knockKeyResendDue(knockKeySentMarks[hex], version.toInt(), members)) return
            val envelope = uniffi.lakko_ffi_uniffi.composeKnockKeyEnvelope(version, pk)
            // A behind device's control send answers `null` (skipped): never
            // taken as sent.
            if (broadcastControlToGroup(identity, groupIdHex, record.group, envelope) != null) {
                knockKeySentMarks[hex] = KnockKeySentMark(version.toInt(), members)
            }
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "knock key not sent (${errorLogTag(cause)})")
        }
    }

    /**
     * Section 4 — this device brought another back: the state a newcomer is
     * handed on growth is re-sent at once (the group name, the settings, the
     * shared pins, the push routing secret, the open items, and the knock key,
     * which an admin holding the seeds sends too). Every member's own profile
     * the returning device asks for itself on joining.
     */
    private suspend fun resendGrowthStateAfterReentry(groupIdHex: String) {
        try {
            groupNameRepublish.publishNow(groupIdHex)
            groupSettingsRepublish.publishNow(groupIdHex)
            pinRepublish.onGrowth(groupIdHex)
            // The push routing secret only from the owner or an admin, as on
            // an ordinary growth: a plain member's is refused by the others'
            // gate anyway, and the re-send would only be noise.
            if (ownIsOwnerOrAdmin(groupIdHex)) redistributePushRoutingBestEffort(groupIdHex)
            handOverOpenItemsBestEffort(groupIdHex, 1)
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "growth state not re-sent (${errorLogTag(cause)})")
        }
        // Cleared at re-entry: the replacement re-sends it. A returning admin
        // could open none of the seeds sent while it was behind: its marks go
        // too, so the next delivery sends it every version.
        knockKeySentMarks.remove(groupIdHex.lowercase())
        forgetKnockSeedMarks(groupIdHex)
        distributeKnockKeyIfDueBestEffort(groupIdHex, afterReentry = true)
    }

    /**
     * Walk 3, W3-R5 — this device came back in step in [groupIdHex] (by a
     * read, or the re-entry joined): what it sent while it was behind may
     * have been unreadable, and what it held back was never sent. It re-sends
     * its own growth-time state there, through the same re-sends a
     * newcomer's growth triggers: the profile, the group name, the photo /
     * reactions setting / handle (each only where this device holds it and
     * may send it; an owner's handle stored first is carried here), the
     * shared pins, the shared channel list, and the knock key as owner or
     * admin.
     */
    private suspend fun resendOwnGrowthStateBestEffort(groupIdHex: String) {
        if (groups[groupIdHex] == null) return
        try {
            profileRepublish.publishNow(groupIdHex)
            groupNameRepublish.publishNow(groupIdHex)
            groupSettingsRepublish.publishNow(groupIdHex)
            pinRepublish.onGrowth(groupIdHex)
            sharedChannels.groupGrew(groupIdHex)
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "own state not re-sent (${errorLogTag(cause)})")
        }
        // The knock key: an owner always, an admin as after a re-entry; the
        // sent marks go so it is sent whatever this device sent before.
        knockKeySentMarks.remove(groupIdHex.lowercase())
        distributeKnockKeyIfDueBestEffort(groupIdHex, afterReentry = true)
    }

    /** Whether this account is [groupIdHex]'s owner or one of its admins,
     *  read now; `false` when the group or its role table cannot be read. */
    private suspend fun ownIsOwnerOrAdmin(groupIdHex: String): Boolean {
        val record = groups[groupIdHex] ?: return false
        return providerLock.sessionLock {
            val own = ownAccountKey()
            val table = roleTableOrNull(record.group)
            val isAdmin = own != null && table?.admins?.any { it.contentEquals(own) } == true
            reentryResendsPushRoutingSecret(ownsGroupAsOwner(record.group), isAdmin)
        }
    }

    /** Section 3 route 3 — a knock key from [senderKey], kept only from the
     *  owner or an admin resolved at receipt, and only when strictly newer.
     *  Inside the poll's locked apply region. */
    private fun receiveKnockKey(group: Group, groupIdHex: String, senderKey: MemberIdentity?, knock: uniffi.lakko_ffi_uniffi.FfiKnockKey) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val sender = senderKey?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        val table = roleTableOrNull(group)
        val privileged = sender != null && table != null &&
            (table.owner.contentEquals(sender) || table.admins.any { it.contentEquals(sender) })
        val stored = knockKeys.get(accountIdB64, groupIdHex)
        if (!keepKnockKey(privileged, knock.knockVersion.toInt(), stored) { incoming, held ->
                uniffi.lakko_ffi_uniffi.knockKeyIsNewer(incoming.toUShort(), held?.toUShort())
            }
        ) {
            return
        }
        knockKeys.put(accountIdB64, groupIdHex, HeldKnockKey(knock.knockVersion.toInt(), outboxB64(knock.knockPk)))
    }

    /** Section 3 route 3 — a re-entry request read from the knock queue by
     *  an owner or admin: opened with the seed of the row's own version. */
    private fun openReentryKnockRowOrNull(record: GroupRecord, groupIdHex: String, bytes: ByteArray): ByteArray? {
        val version = try {
            uniffi.lakko_ffi_uniffi.knockRowVersion(bytes)
        } catch (_: Throwable) {
            null
        } ?: return null
        val seed = knockSeedFor(record, groupIdHex, version) ?: return null
        return try {
            val opened = uniffi.lakko_ffi_uniffi.openReentryKnockRow(seed, bytes)
            uniffi.lakko_ffi_uniffi.composeConversationReentryEnvelope(
                opened.groupId,
                opened.epochLeft,
                opened.gapFromMs,
                opened.keyPackage,
            )
        } catch (_: Throwable) {
            null
        } finally {
            seed.fill(0)
        }
    }

    /** The re-entry KeyPackages this device recorded for [groupIdHex]. */
    private fun recordedReentryKeyPackages(groupIdHex: String?): List<ByteArray> {
        val hex = groupIdHex ?: return emptyList()
        val accountIdB64 = currentAccountIdB64.get() ?: return emptyList()
        return try {
            reentryRecords.get(accountIdB64, hex)?.requests.orEmpty().map { outboxUnb64(it.keyPackageB64) }
        } catch (_: Throwable) {
            emptyList()
        }
    }

    /**
     * Section 5 — this device came back into [groupIdHex]: the conversation
     * and its history stay, the suspicion and the record go, and the gap line
     * is written. Then the messages of the gap are fetched from the
     * rendezvous the re-admitter posts them to.
     */
    private fun onReentryLanded(groupIdHex: String, joined: Group) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val identity = canonicalIdentity.get() ?: return
        val hex = groupIdHex.lowercase()
        val record = reentryRecords.get(accountIdB64, hex)
        val route = record?.requests?.lastOrNull()?.route ?: ReentryRouteKind.KNOCK
        // Walk 2, R1 — the line starts where the request said the gap did
        // (the earliest a recorded request carried), never later than the
        // gap start known now.
        val gapNow = conversationBehind.gapStartMs(hex)
        val gapFrom = record?.requests?.minOfOrNull { it.gapFromMs }?.let { minOf(it, gapNow) } ?: gapNow
        val now = System.currentTimeMillis()
        val anchor = groups[groupIdHex]?.snapshot()?.maxOfOrNull { it.messageId }?.coerceAtLeast(0L) ?: 0L
        loadReentryGaps(hex)
        saveReentryGaps(hex, withGap(reentryGaps.value[hex].orEmpty(), ReentryGap(gapFrom, now, anchor)))
        conversationBehind.cameBack(hex, now)
        reentryRecords.remove(accountIdB64, hex)
        android.util.Log.i(REENTRY_LOG_TAG, "back in the conversation: route=${route.name.lowercase()}")
        // The secret is of the epoch the device joined at, which is the one
        // the re-admitter exports at: derived now, before any later commit.
        val secret = try {
            joined.exportSecret(identity, REENTRY_HISTORY_EXPORTER_LABEL, identity.signaturePublicKey(), 32u)
        } catch (_: Throwable) {
            null
        }
        if (secret != null) {
            // Section 7 — a durable look, retried from every applied poll of
            // this group and on opening the chat, at most once a minute, for
            // 30 days: the re-admitter may send it much later than the join.
            val started = try {
                gapHistoryLooker.start(
                    accountIdB64,
                    GapLookRecord(
                        groupIdHex = hex,
                        secretB64 = java.util.Base64.getEncoder().encodeToString(secret),
                        gapFromMs = gapFrom,
                        gapToMs = now,
                        route = route,
                    ),
                )
            } catch (ended: me.lakko.app.StaleBindingException) {
                throw ended
            } catch (cause: Throwable) {
                android.util.Log.w(REENTRY_LOG_TAG, "gap look not saved (${cause.javaClass.simpleName})")
                false
            } finally {
                secret.fill(0)
            }
            if (started) pushScope.launchSession { lookForGapHistoryBestEffort(hex) }
        }
    }

    /** Section 7 — one look for [groupIdHex]'s gap bundle, launched (the
     *  chat opened). Throttled by the record itself. */
    internal fun lookForGapHistoryOnOpen(groupIdHex: String) {
        if (currentAccountIdB64.get() == null) return
        pushScope.launchSession { lookForGapHistoryBestEffort(groupIdHex) }
    }

    /**
     * Section 7 — the returning device's look for the messages it missed: one
     * short read of the rendezvous the re-admitter posts to, when the look is
     * due. The first row that opens is applied, the gap line narrowed and the
     * look dropped; anything else keeps it for the next look.
     */
    private suspend fun lookForGapHistoryBestEffort(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val hex = groupIdHex.lowercase()
        // Walk 3, W3-R3 — this account's own key in the conversation: a
        // bundled row it authored is shown as this device's own.
        val ownKey = groups[hex]?.let { record ->
            try {
                providerLock.sessionLock { ownIdentity(record.group) }
            } catch (ended: me.lakko.app.StaleBindingException) {
                throw ended
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                null
            }
        }
        val outcome = try {
            gapHistoryLooker.lookIfDue(
                accountIdB64 = accountIdB64,
                groupIdHex = hex,
                read = { rendezvous ->
                    val fetched = dsClient.fetchRendezvous(rendezvous, after = 0L, waitSecs = 0)
                    ensureSession()
                    fetched.messages.map { it.payload }
                },
                open = { secret, payload, route ->
                    me.lakko.app.devices.HistoryBundleCodec.applyReentryBundle(
                        applicationContext,
                        accountIdB64,
                        secret,
                        payload,
                        hex,
                    ) { row, author -> reentryBundleRow(row, author, ownKey, route) }
                },
                received = { record, applied ->
                    // From the store, not the in-memory flow: after a
                    // restart this chat's lines may not be loaded yet.
                    val gaps = try {
                        reentryGapStore.load(accountIdB64, hex)
                    } catch (_: Throwable) {
                        emptyList()
                    }
                    val narrowed = gaps.mapNotNull { gap ->
                        if (gap.fromMs == record.gapFromMs && gap.toMs == record.gapToMs) {
                            gapAfterBundle(gap, applied.rowTimesMs, applied.capped)
                        } else {
                            gap
                        }
                    }
                    saveReentryGaps(hex, narrowed)
                    android.util.Log.i(REENTRY_LOG_TAG, "missed messages received: rows=${applied.applied}")
                },
            )
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            android.util.Log.w(REENTRY_LOG_TAG, "gap look failed (${errorLogTag(cause)})")
            return
        }
        if (outcome == GapLookOutcome.DROPPED) {
            android.util.Log.i(REENTRY_LOG_TAG, "gap look dropped: retention passed")
        }
    }

    /**
     * §6.2.4 — `Delete this chat`, on what stays ended on this device: a
     * one-to-one, or a group whose account is still in it on another device.
     *
     * The same walk as an end ([runMembershipEnd]), from the wipe on: every
     * segment of the conversation, its stores, the MLS group, the provider
     * snapshot, and the known-groups row and the record last. The row goes at
     * once; the rest resumes after a crash. No register and no tombstone, since
     * the account's other devices are not in the state this one is.
     *
     * The push subscription is already gone by this point: the §6.2.8 teardown
     * retires it at the moment of the eviction, not at the moment of the
     * delete, so a user who never presses this button still stops being woken.
     */
    public fun deleteEndedChat(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val hex = groupIdHex.lowercase()
        if (!membershipEnds.save(accountIdB64, chosenEnd(hex, 0u))) return
        endingGroups += hex
        publishThreadList()
        pushScope.launchSession { runOwedMembershipEnds() }
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
    /** What the members panel shows, in a form that changes when it does: the
     *  roster, the role table and the pending set. `null` when not readable. */
    public fun membersPanelSignature(groupIdHex: String): List<String>? {
        val record = groups[groupIdHex] ?: return null
        return try {
            val roles = record.group.groupRoles()
            buildList {
                record.group.members().forEach { add(outboxB64(it.signaturePublicKey)) }
                add("owner:" + (roles?.owner?.let(::outboxB64) ?: ""))
                roles?.admins?.forEach { add("admin:" + outboxB64(it)) }
                add("pending:" + record.group.pendingProposals().size)
                add("joins:" + record.group.pendingExternalJoins().size)
            }
        } catch (_: Throwable) {
            null
        }
    }

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
            providerLock.sessionLock {
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
        forSession { emitCommitterBeaconIfDueOnMlsThread(groupIdHex = groupIdHex, foreground = foreground) }

    private suspend fun emitCommitterBeaconIfDueOnMlsThread(
        groupIdHex: String,
        foreground: Boolean,
    ): String {
        // Short-circuited FIRST so a backgrounded app does no work at all — the
        // same answer [beaconEmitVerdict] gives below, reached before the
        // Ed25519 key derivation the rank read costs. The gate there stays the
        // authority.
        if (!foreground) return BeaconEmitVerdict.NOT_FOREGROUND.name
        // ADR-0091 phase 2 section 2 / C-64 — a device behind in this
        // conversation emits no beacon: it would claim a sweep it cannot make.
        // Walk 3, W3-R2 — nor while a read of the journal was refused.
        if (behindHoldsAutomaticSends(groupIdHex)) return BeaconEmitVerdict.NOT_A_CANDIDATE.name
        val identity = canonicalIdentity.get() ?: return BeaconEmitVerdict.NOT_A_CANDIDATE.name
        val record = groups[groupIdHex] ?: return BeaconEmitVerdict.NOT_A_CANDIDATE.name
        var canRegister = false
        val ownRank = try {
            providerLock.sessionLock {
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
            // ticker harmless rather than merely unlikely. ADR-0087 section 9:
            // a group left on another device's word still reads active here.
            membershipEnded = readIsActive { record.group.isActive() } == false ||
                groupIdHex.lowercase() in endingGroups,
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
        forSession { sweepIfCommitterOnMlsThread(groupIdHex = groupIdHex) }

    private suspend fun sweepIfCommitterOnMlsThread(groupIdHex: String): SweepOutcome? {
        // ADR-0073 R.7 case 1 — a dead one-to-one commits nothing.
        if (deadOneToOne(groupIdHex)) return null
        // ADR-0091 phase 2 section 2 — nor does a device behind there.
        if (behindStopsActing(groupIdHex)) return null
        // ADR-0087 section 9 — and nor does a group this account is out of.
        if (groupIdHex.lowercase() in endingGroups) return null
        val identity = canonicalIdentity.get() ?: return null
        val record = groups[groupIdHex] ?: return null
        if (currentAccountIdB64.get() == null) return null
        // ADR-0043 — the baseline for the membership diff, captured INSIDE the
        // group lock immediately before the sweep, so a commit landing in the
        // window cannot race the shrink diff. Lock order is always group first,
        // provider second.
        var membersBefore: List<String>? = null
        var accountKeysBeforeSweep: List<String>? = null
        val outcome = try {
            groupLock(groupIdHex).sessionLock {
                MainThreadGuard.assertOffMain("sweepIfCommitter")
                var mayCommit = true
                // ADR-0057 S2 finding 1 — the owner before the sweep, so a
                // merged sweep (or a remote commit its verdict applied) that
                // carried this account's ownership transfer is seen.
                var ownerBeforeSweep: ByteArray? = null
                membersBefore = providerLock.sessionLock {
                    // UX second-device.md section 4 — the people before the
                    // sweep: this device never receives its own commit back,
                    // so it writes the named row itself once the commit merges.
                    accountKeysBeforeSweep = memberAccountKeysOrNull(record.group)
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
                    runCommitterSweep(sweepPortsFor(record, identity, groupIdHex, callBinding()))
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
        recordOwnCommitRowBestEffort(record, groupIdHex, outcome.commitMessageId, accountKeysBeforeSweep)
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
        val membersAfterSweep = providerLock.sessionLock { memberSignatureKeysOrNull(record.group) }
        val evictedBySweep = readIsActive { record.group.isActive() } == false
        if (evictedBySweep) dropRestoredKnownGroupBestEffort(groupIdHex)
        // ADR-0087 section 9 — the seam on the path that merges its own commit.
        if (evictedBySweep) providerLock.sessionLock { recordEndIfOutLocked(groupIdHex, record) }
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
            ownAdd = outcome.addedCount > 0,
        )
        // ADR-0042 — a swept ADD needs the current routing secret, exactly as
        // the direct add path re-broadcasts it. No-op when this device holds
        // none.
        if (outcome.addedCount > 0) redistributePushRoutingBestEffort(groupIdHex)
        if (groupIdHex.lowercase() in endingGroups) runOwedMembershipEnds()
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
        /** The binding of the call the sweep runs in. */
        bound: me.lakko.app.Binding,
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
                    // The people before the row, so a swept commit that only
                    // changes devices draws no `Members changed` row.
                    val accountKeysBeforeRow = memberAccountKeysOrNull(record.group)
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
                        // R.5 / R.8 — the same drop as the poll's.
                        val sweptEvents = withoutSupersededSenders(record.group, groupIdHex, listOf(sweptEvent))
                        val accountKeysAfterRow = memberAccountKeysOrNull(record.group)
                        val sweptSuppressed = sweptEvents.any {
                            it is IncomingChatEvent.System && it.systemKind == SystemKind.Commit
                        } && suppressMembersChangedRow(groupIdHex, accountKeysBeforeRow, accountKeysAfterRow)
                        record.consumeEvents(
                            events = sweptEvents,
                            heldResend = heldResendPredicate(groupIdHex, sweptEvents),
                            suppressCommitRows = sweptSuppressed,
                            membershipChange = membershipRowChange(
                                groupIdHex,
                                sweptEvents,
                                accountKeysBeforeRow,
                                accountKeysAfterRow,
                                sweptSuppressed,
                            ),
                            callSignalingSink = { messageId, payload, createdAt, senderLeaf, senderKey, senderPersonKey ->
                                routeIncomingCallSignaling(
                                    groupIdHex, messageId, payload, createdAt, senderLeaf, senderKey, senderPersonKey,
                                    record.group,
                                )
                            },
                            profileSink = { senderKey, profile ->
                                receiveProfileBestEffort(senderKey, profile)
                                receiveProfileGrantsBestEffort(record.group, groupIdHex, senderKey, profile)
                            },
                            scopedSink = { senderKey, kind, bytes ->
                                receiveScopedEnvelopeBestEffort(record.group, groupIdHex, senderKey, kind, bytes)
                            },
                            groupSettingSink = { senderKey, kind, bytes ->
                                receiveGroupSettingBestEffort(record.group, groupIdHex, senderKey, kind, bytes)
                            },
                            groupNameSink = { senderKey, name ->
                                receiveGroupName(record.group, groupIdHex, senderKey, name)
                            },
                            groupClosedSink = { messageId, senderKey, closed ->
                                receiveSuccessorClaim(record.group, groupIdHex, messageId, senderKey, closed)
                            },
                            pinSink = { senderKey, op ->
                                receivePin(record.group, groupIdHex, senderKey, op)
                            },
                            sharedChannelsSink = { senderKey, list ->
                                sharedChannels.receive(groupIdHex, senderKey, list)
                            },
                            registrarSeedSink = { senderKey, envelope ->
                                receiveRegistrarSeed(record.group, groupIdHex, senderKey, envelope)
                            },
                            groupContactRequestSink = { senderKey, envelope, messageId, createdAt ->
                                receiveGroupContactRequest(groupIdHex, senderKey, envelope, messageId, createdAt)
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
                        persistReadWatermarksIfReceipts(groupIdHex, record, sweptEvents)
                        settleHeldCopies(groupIdHex, record, sweptEvents)
                        // A swept chat message counts toward the chat list's
                        // unread badge exactly as a polled one does.
                        localUnread.onIncomingApplied(
                            groupIdHex,
                            sweptEvents.filterIsInstance<IncomingChatEvent.Application>().map { it.messageId },
                            record.syncedThroughMessageId,
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

                // ADR-0081 decision 1 — the staged commit and the registration
                // of the epoch it moves to, in one request proven under N.
                override suspend fun commitStaged(bytes: ByteArray, staged: StagedSweepArtifacts?): Long =
                    commitStagedSweep(record, groupIdHex, identity, bytes, staged)

                // ADR-0050 section 2.5a item 7 — never unsigned: with no
                // grant accepted the Welcome stays owed.
                override suspend fun publishMailbox(pseudonym: ByteArray, bytes: ByteArray): Long =
                    publishSweepWelcome(account = pseudonym, bytes = bytes, groupIdHex = groupIdHex)
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
                        // Refused when the session has ended, which the
                        // sweep must hear as "not recorded".
                        me.lakko.app.account.AccountStoreGate.writeOrEnd(accountIdB64) {
                            CommitOutboxStore.markWelcomesOwed(
                                applicationContext,
                                id,
                                commitMessageId,
                            )
                        }
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
                    // Walk 2 — an acked sweep entry owes nothing (its verdict
                    // merged, its Welcomes are out) and nothing reads it, so
                    // it is deleted rather than kept as an acked file. A
                    // failed delete falls back to the ack mark, which the
                    // next open collects.
                    val deleted = try {
                        outboxWrite { CommitOutboxStore.deleteEntry(applicationContext, id) }
                        true
                    } catch (_: Throwable) {
                        false
                    }
                    if (!deleted) ackCommitBestEffort(id, commitMessageId, welcomeMessageId)
                }

                override suspend fun discard(id: String) {
                    try {
                        outboxWrite { CommitOutboxStore.deleteEntry(applicationContext, id) }
                    } catch (_: Throwable) {
                        // best-effort — the next sweep retries.
                    }
                }
            },
            cursor = object : SweepCursorRef {
                override fun read(): Long = record.cursor

                override fun advance(to: Long) {
                    // Section 6.4 — never past a group contact request that is
                    // not stored yet; the next poll's gate retries the write.
                    try {
                        groupContactRequestsFlush?.invoke()
                    } catch (cause: Throwable) {
                        android.util.Log.w("ChatRepository", "sweep: cursor held, a group contact request is not stored", cause)
                        return
                    }
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
                // ADR-0078: the ACCOUNT keys; each one's mailbox is resolved
                // from a verified binding when it is written.
                welcomeAccounts(leafKeys) { leaf ->
                    roster.firstOrNull { it.signaturePublicKey.contentEquals(leaf) }?.accountKey
                }
            },
            // Only into the echo filter of the session that published: the
            // filter is keyed by journal and row id, and the next account on
            // this phone can be a member of the same group. A row marked
            // there would be a commit that account never applies.
            markOwnPublished = { id -> if (bound.isCurrent) chatClient.markOwnPublishedCommit(groupIdHex, id) },
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
            // ADR-0081 — a 409 is a lost epoch, a refusal that will not change
            // on its own (the backoff's) is definitive; anything else leaves
            // the sweep staged for the owed verdict.
            classifyCommitFailure = { cause ->
                when {
                    cause is me.lakko.app.ds.DsCommitConflictError -> SweepCommitFailure.LOST
                    SweepRegistrationBackoff.isBackoffRefusal(cause) -> SweepCommitFailure.REFUSED
                    else -> SweepCommitFailure.UNKNOWN
                }
            },
            mayStage = {
                val epoch = try {
                    record.group.epoch()
                } catch (_: Throwable) {
                    null
                }
                sweepBackoff.mayStage(groupIdHex, epoch)
            },
            // ADR-0091 phase 1 — the same identifier-free line the poll writes.
            // The sweep's fetch carries no `latest_epoch`.
            logSkippedRow = { row, failure, localEpoch ->
                android.util.Log.w(
                    FAILED_ROW_LOG_TAG,
                    failedRowLine(
                        kind = failedRowKind(failure),
                        sealedEpoch = runCatching { uniffi.lakko_ffi_uniffi.epochOfMessage(row.payload) }.getOrNull(),
                        epochAfter = runCatching { uniffi.lakko_ffi_uniffi.epochAfterMessage(row.payload) }.getOrNull(),
                        localEpoch = localEpoch,
                        latestEpoch = null,
                        verdict = RegisteredCommitVerdict.SKIP,
                        attempt = null,
                    ),
                )
            },
        )
    }

    /**
     * ADR-0081 decision 1 for a STAGED sweep: register the staged epoch's key
     * with this account's root (owner) or registrar signer, revoking the
     * registrar keys of the accounts the sweep evicts (owner only, from
     * [staged]'s removed leaves; nothing on the restart path, where [staged]
     * is `null`), AND append the commit [bytes], in one request proven under
     * the epoch the group still sits at. The signers are derived under the
     * provider lock; the POST runs outside it (the sweep holds only the
     * per-group lock here, and nothing is eager-merged).
     */
    private suspend fun commitStagedSweep(
        record: GroupRecord,
        groupIdHex: String,
        identity: Identity,
        bytes: ByteArray,
        staged: StagedSweepArtifacts?,
    ): Long {
        data class Prepared(
            val next: DsJournalSigner?,
            val current: DsJournalSigner?,
            val statements: me.lakko.app.ds.DsStatementSigning?,
            val revoke: List<ByteArray>,
            val evictees: List<ByteArray>,
            val rank: me.lakko.app.ds.JournalRepairRank,
        )
        // ADR-0057 — a claim of this journal launched on open may still be in
        // flight; commit after it, not ahead of it (bounded).
        ownerClaimGate.awaitBeforeRegistering(groupIdHex)
        val prepared = providerLock.sessionLock {
            val next = record.group.dsStagedAccessSigner(identity)?.let(::FfiDsJournalSigner)
            val evictees = staged?.let {
                me.lakko.app.ds.evictedAccounts(
                    before = rosterLeavesOrNull(record.group).orEmpty(),
                    removedLeaves = it.removedLeaves,
                )
            }.orEmpty()
            Prepared(
                next = next,
                current = currentJournalSigner(groupIdHex),
                statements = commitStatementSignerFor(record, groupIdHex),
                revoke = revokeKeysFor(record, groupIdHex, evictees),
                evictees = evictees,
                rank = journalRepairRankFor(record, groupIdHex),
            )
        }
        val answer = try {
            noteAppendConflict(groupIdHex, record.groupId, prepared.current, prepared.rank) {
                me.lakko.app.ds.commitEpochWith(
                    journalId = record.groupId,
                    before = prepared.current,
                    after = prepared.next,
                    statements = prepared.statements,
                    revoke = prepared.revoke,
                    row = bytes,
                    post = { make, proof -> dsClient.postCommit(make, proof) },
                )
            }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            // A refusal that will not change on its own holds the next stage
            // of this group. Since ADR-0050 section 2.5a item 8 that includes a
            // commit this device could not even sign (`DsRegistrationUnavailable`).
            if (SweepRegistrationBackoff.isBackoffRefusal(cause)) {
                val hold = sweepBackoff.onRefused(groupIdHex, prepared.current?.epoch)
                android.util.Log.i(
                    "ChatRepository",
                    "ADR-0057: sweep commit refused (${cause.javaClass.simpleName}); " +
                        "next attempt in ${hold.delayMs} ms",
                )
            }
            throw cause
        }
        // The appointment records are rewritten under the signed-in account:
        // the one that committed, or nothing.
        ensureSession()
        sweepBackoff.reset(groupIdHex)
        forgetRevokedAppointments(groupIdHex, prepared.evictees, prepared.revoke)
        return answer.id
    }

    public suspend fun sweepPendingCommits(groupIdHex: String): Unit =
        forSession { sweepPendingCommitsOnMlsThread(groupIdHex = groupIdHex) }

    private suspend fun sweepPendingCommitsOnMlsThread(groupIdHex: String): Unit {
        // ADR-0073 R.7 case 1 — a dead one-to-one resolves nothing it owed:
        // the owed commit / Welcome re-publish is a send into a session whose
        // other leaves are removed devices. A sweep left staged only freezes
        // this group's own membership operations, which it never runs again.
        if (deadOneToOne(groupIdHex)) return
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
                groupLock(groupIdHex).sessionLock {
                    resolveOwedVerdict(sweepPortsFor(record, identity, groupIdHex, callBinding()))
                }
            } catch (cause: Throwable) {
                // An unreadable verdict leaves the sweep staged for the next
                // attempt; the poll tail retries continuously.
                android.util.Log.w("ChatRepository", "ADR-0045 owed verdict unresolved", cause)
            }
            // ADR-0087 section 9 — that verdict may have merged this account's
            // own removal.
            providerLock.sessionLock { recordEndIfOutLocked(groupIdHex, record) }
            if (groupIdHex.lowercase() in endingGroups) {
                runOwedMembershipEnds()
                return
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
                groupLock(groupIdHex).sessionLock {
                    resolveOwedWelcomes(sweepPortsFor(record, identity, groupIdHex, callBinding()))
                }
            } catch (cause: Throwable) {
                // The DS is still unreachable. The durable marker survives, so
                // the next sweep — group open or poll tail — retries the fan-out.
                android.util.Log.w("ChatRepository", "ADR-0045 owed Welcome unresolved", cause)
            }
        }
        val all = try {
            CommitOutboxStore.loadForGroup(applicationContext, accountIdB64, groupIdB64)
        } catch (_: Throwable) {
            return
        }
        // Walk 2 — an ACKED sweep entry owes nothing: every reader of a sweep
        // entry ([resolveOwedVerdict]'s finder, the Welcome fan-out finders)
        // asks for an un-acked one, and the legacy loop below never sees
        // sweep entries. Such entries were never collected and piled up, one
        // per sweep. Collected here (entries written before the sweep ack
        // started deleting them).
        for (entry in all) {
            if (!collectableSweepEntry(entry)) continue
            try {
                outboxWrite { CommitOutboxStore.deleteEntry(applicationContext, entry.id) }
            } catch (_: Throwable) {
                // best-effort GC — the next open retries.
            }
        }
        // ADR-0045 — a sweep entry belongs to `resolveOwedVerdict` above,
        // never to the direct-path reconciler.
        val entries = all.filterNot { it.isSweep }
        if (entries.isEmpty()) return
        // ADR-0060 §6 — the account each owed Welcome is addressed to, so the
        // re-publish picks that person's reach grant.
        for (entry in entries) {
            val mailbox = entry.secondArtifactMailboxB64 ?: continue
            val account = entry.secondArtifactAccountKeyB64 ?: continue
            try {
                // ADR-0074 3.5a — the recorded key is what the leaf names;
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
        val bound = callBinding()
        groupLock(groupIdHex).sessionLock {
            val currentEpoch = try {
                providerLock.sessionLock { record.group.epoch() }
            } catch (_: Throwable) {
                return@sessionLock
            }
            sweepPendingCommitEntries(
                entries = entries,
                currentEpoch = currentEpoch,
                republishWelcome = { mailbox, bytes -> publishWelcomeToMailbox(mailbox, bytes, groupIdHex).id },
                onAck = { id, commitId, secondId -> ackCommitBestEffort(id, commitId, secondId) },
                onDiscard = { id ->
                    try {
                        outboxWrite { CommitOutboxStore.deleteEntry(applicationContext, id) }
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
        pushScope.launchSession {
            val snapshot = record.readUpToSnapshot()
            if (snapshot.isEmpty()) return@launchSession
            if (currentAccountIdB64.get() != accountIdB64) return@launchSession
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
     *
     * The row sits in the conversation at the moment the call ended; the Calls
     * list files it under [startedAtMs], when it started (section 6.2), or
     * the end when the host never saw it start.
     */
    public fun recordCallRow(
        groupIdHex: String,
        entry: me.lakko.app.calls.CallTranscriptEntry,
        startedAtMs: Long? = null,
    ) {
        val record = groups[groupIdHex] ?: return
        val accountIdB64 = currentAccountIdB64.get()
        // The one-time fill from past calls must see the transcripts as they
        // were BEFORE this call's row: it files each row it finds and the index
        // keeps no ids, so a fill that runs after a live record (the first
        // opening of the Calls screen used to be what ran it) files that call a
        // second time. Seen on the emulator on 2026-10-23: each call listed
        // twice. The snapshot is taken here, before the append; the disk work
        // stays off this thread.
        val pastBeforeThisCall = takeCallHistoryFillSnapshot(accountIdB64)
        val now = java.time.Instant.now()
        record.appendCallRow(entry, createdAt = now.toString())
        publishThreadList()
        // docs/ux/navigation.md section 6.2 — the Calls screen reads an index
        // written when the row is, never a walk of every transcript. Written on
        // the same dispatcher as the transcript persist and after it, so a
        // failure here can never cost the conversation its row.
        // The account the call was placed under, read above and not when the
        // write runs.
        val filedAtMs = startedAtMs ?: now.toEpochMilli()
        callRowScope.launchSession {
            persistMessageLogBestEffort()
            if (pastBeforeThisCall != null) {
                me.lakko.app.calls.CallHistoryStore.backfillOnce(
                    context = applicationContext,
                    accountIdB64 = accountIdB64,
                ) { pastBeforeThisCall }
            }
            recordCallHistoryEntry(accountIdB64, groupIdHex, entry, filedAtMs)
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
        accountIdB64: String?,
        groupIdHex: String,
        entry: me.lakko.app.calls.CallTranscriptEntry,
        startedAtMs: Long,
    ) {
        try {
            me.lakko.app.calls.CallHistoryStore.record(
                context = applicationContext,
                accountIdB64 = accountIdB64,
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
        val past = takeCallHistoryFillSnapshot(accountIdB64) ?: return
        callRowScope.launchSession {
            me.lakko.app.calls.CallHistoryStore.backfillOnce(
                context = applicationContext,
                accountIdB64 = accountIdB64,
            ) { past }
        }
    }

    /** Accounts whose fill snapshot this process has taken. */
    private val callHistoryFillSnapshotTaken: MutableSet<String> =
        java.util.Collections.synchronizedSet(HashSet())

    /**
     * The past calls the one-time fill files, read now - on the main thread,
     * in order with the call rows it appends - and at most once per account
     * per process: the first call to finish or the first opening of the Calls
     * screen, whichever comes first, takes it, and nothing later can file a
     * row whose call was already recorded live. `null` when it was already
     * taken. [me.lakko.app.calls.CallHistoryStore.backfillOnce] still runs the
     * fill once per account ever.
     */
    private fun takeCallHistoryFillSnapshot(accountIdB64: String?): List<me.lakko.app.calls.CallHistoryEntry>? {
        if (accountIdB64 == null || !callHistoryFillSnapshotTaken.add(accountIdB64)) return null
        return pastCallEntries()
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
        var accountIdB64: String? = null
        val slots = mutableMapOf<String, PersistedGroup>()
        var waiting: Map<String, PersistedGroup> = emptyMap()
        // G-37 — `groups` is a `synchronizedMap`, whose iterators are NOT
        // self-synchronising: iterating it while another coroutine registers a
        // group can throw ConcurrentModificationException. The old
        // repository-wide mutex made this accidentally safe (every writer held
        // it); per-group locking removes that accident, so the map's own
        // monitor is taken explicitly — matching what `pollAllGroups` and
        // `publishThreadList` already do.
        synchronized(groups) {
            // The account and the records are read under ONE monitor, the one
            // [wipeInMemoryState] empties both under: a write is either this
            // account's whole log or nothing.
            accountIdB64 = messageLogAccount
            if (accountIdB64 == null) return
            for ((hex, record) in groups) {
                slots[hex] = PersistedGroup(
                    cursor = record.cursor,
                    messages = persistableMessages(record.snapshot()),
                    feedSeen = record.feedSeen.sorted(),
                    feedSeenThrough = record.feedSeenThrough,
                    syncedThroughMessageId = record.syncedThroughMessageId,
                )
            }
            waiting = waitingLogSlots.snapshot()
        }
        MessageLogStore.putLog(
            context = applicationContext,
            accountIdB64 = accountIdB64 ?: return,
            payload = MessageLogPayload(groups = composeMessageLog(slots, waiting)),
        )
    }

    /**
     * A joined group goes into [groups] together with what the log holds for
     * it, under one monitor: the persist that follows a join writes the whole
     * log, and a record registered empty would write this group's history
     * bundle away.
     */
    private fun registerJoinedGroup(groupIdHex: String, record: GroupRecord) {
        // ADR-0087 section 9 — a group this account came back to starts empty,
        // so a slot a stale log write left behind is not history for it.
        val cameBack = groupRefOf(groupIdHex)?.let {
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.isConversationEnded(it)
        } == true
        val replaced = synchronized(groups) {
            val previous = groups.put(groupIdHex, record)
            val slot = waitingLogSlots.take(groupIdHex)
            if (!cameBack && previous == null) slot?.let { record.adoptHistory(it) }
            previous
        }
        // The ended group's handle: nothing reads it any more, and every poll
        // that held it checks the map before it applies.
        if (replaced != null && replaced !== record) {
            try {
                replaced.group.close()
            } catch (_: Throwable) {
                // best-effort — releasing the stale Rust Arc must not fail a join.
            }
        }
    }

    /**
     * A history bundle landed while this account's chat layer runs (a device
     * pairing while signed in, or one whose Welcomes came first): the rows go
     * to the record of a group this device holds, else they wait for its
     * join. [slots] are the bundle's own rows, so a persist that ran since the
     * bundle reached the disk cost nothing.
     */
    private fun adoptHistoryBundle(accountIdB64: String, slots: Map<String, PersistedGroup>) {
        synchronized(groups) {
            if (messageLogAccount != accountIdB64) return
            for ((groupIdHex, slot) in slots) {
                val record = groups[groupIdHex]
                if (record == null) {
                    waitingLogSlots.add(groupIdHex, slot)
                } else {
                    record.adoptHistory(slot)
                    slot.syncedThroughMessageId?.let { localUnread.onSyncedThrough(groupIdHex, it) }
                }
            }
        }
        persistMessageLogBestEffort()
        publishThreadList()
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
        // ADR-0087 section 9 — the row goes the moment the end is recorded,
        // whatever the steps behind it still have to do.
        val records = synchronized(groups) { groups.values.toList() }
            .filter { DsClient.pseudonymHex(it.groupId).lowercase() !in endingGroups }
        val leaveAccount = currentAccountIdB64.get()
        val threads = records.map { record ->
            val thread = record.toThread()
            val unread = localUnread.snapshot(thread.groupIdHex)
            thread.copy(
                leaveWaiting = !thread.membershipEnded && leaveAccount != null &&
                    loadLeaveRequestedAt(leaveRecords, leaveAccount, thread.groupIdHex) != null,
                joinedAtMs = leaveAccount?.let {
                    ConversationJoinedStore.load(applicationContext, it, thread.groupIdHex)
                },
                localUnread = unread.count,
                unreadViewedThisSession = unread.viewedThisSession,
            )
        }
        val conversations = try {
            foldConversations(threads, lineageNow())
        } catch (_: Throwable) {
            threads
        }
        mutableState.update { it.copy(threads = threads, conversations = conversations) }
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
        // ADR-0073 R.5 — the screen shows the whole conversation: the older
        // segments are on screen with the group it is open on.
        for (hex in olderSegmentsOf(groupIdHex)) localUnread.setOnScreen(hex, onScreen)
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
    ): Map<String, Long> = forSession {
        // The ids in [seen] were shown to the account that called. The groups
        // are looked up again after every lock wait, and a group id alone
        // does not say whose record it is.
        val advanced = HashMap<String, Long>()
        var changed = false
        for ((hex, ids) in seen) {
            val record = groups[hex] ?: continue
            groupLock(hex).sessionLock {
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
                if (result.newlySeen > 0) localUnread.onSeenElsewhere(hex, ids)
                result.advancedTo?.let { advanced[hex] = it }
                changed = true
            }
        }
        if (changed) {
            persistMessageLogBestEffort()
            publishThreadList()
        }
        advanced
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
    /**
     * ADR-0073 R.5 — one thread per CONVERSATION ([foldConversations] over
     * [threads]): what the chat list, the pickers and an open chat show. A
     * conversation continued in a successor group is one entry here, with the
     * older segments as read-only history above the head's rows. [threads]
     * stays one entry per MLS group for everything that walks groups.
     */
    val conversations: List<ChatThread> = emptyList(),
    /** ADR-0087 section 9 — "You are no longer in {name}.", each owed once on
     *  this device. */
    val endNotices: List<MembershipEndNotice> = emptyList(),
    /** ADR-0087 section 9 — the conversations on their way out on this device
     *  (lower-case hex): gone from every list, with their steps still running. */
    val endingIds: Set<String> = emptySet(),
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
    /** ADR-0087 UX 9.4 — this device asked to leave and the request has not
     *  landed: the row says so. */
    val leaveWaiting: Boolean = false,
    /** The moment this device created or joined this group
     *  ([ConversationJoinedStore]), which is what the list sorts a
     *  conversation with no message by. Carried here so a thread that gains it
     *  counts as changed and the row is built again. */
    val joinedAtMs: Long? = null,
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
    /** `docs/ux/second-device.md` section 2 — the pairing-history floor per
     *  group of this thread (lower-case hex; a folded thread holds its older
     *  segments' too): a row at or below it is never unread on this device
     *  ([rowInPairingHistory]). Local, never sent. */
    val syncedThrough: Map<String, Long> = emptyMap(),
    /** ADR-0073 R.5 — the conversation this group belongs to (the hex of the
     *  lineage's first group): the key of every person-facing store. */
    val conversationHex: String = groupIdHex,
    /** ADR-0073 R.5 — in a folded thread ([foldConversations]), the older
     *  segments whose rows precede the head's, oldest first. */
    val olderSegments: List<String> = emptyList(),
    /** ADR-0073 R.5 — the index of THIS group among the conversation's
     *  segments, as the lineage orders them (0 for a conversation that never
     *  moved). A read position names a segment by this index, and a device
     *  that joined at a successor holds no earlier segment, so the index is
     *  the lineage's and not a position in [olderSegments]. */
    val segmentIndex: Int = 0,
    /** The lineage index of each of [olderSegments], in the same order. */
    val olderSegmentIndices: List<Int> = emptyList(),
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
    /** What is held after the fold of [reactionOps] (reactions-and-emoji
     *  section 10): per person, at most three, ordered by the add. */
    val reactions: List<Reaction> = emptyList(),
    /** The reaction ops that reached this message, in DS order. */
    val reactionOps: List<ReactionOpRecord> = emptyList(),
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
    /**
     * ADR-0087 section 14 — the channel invitation card the message carries
     * beside its text (`openChannelInviteCard`), `null` for every other row.
     */
    val channelInvite: ByteArray? = null,
    /**
     * ADR-0073 R.5 — set on a row of an OLDER segment of the conversation, as
     * the folded timeline shows it ([foldConversations]): the hex id of the
     * group the row is stored in. Such a row is read-only history; its
     * [messageId] is a negative on-screen id ([historyRowId]) and
     * [historyMessageId] is the DS id it has in that group. Never stored.
     */
    val historyOf: String? = null,
    val historyMessageId: Long = 0,
    /**
     * ADR-0082 section 5 — the DS id of the last edit or delete applied to this
     * message, `0` when none. A stored translation or transcript carries the
     * value it was made for; a higher one makes it stale. An own edit whose
     * publish returned no id still moves it ([nextLastOpId]).
     */
    val lastOpId: Long = 0,
    /**
     * `docs/ux/second-device.md` section 4 — on a group's commit row, the
     * person keys the receive batch it closed added ([membershipChangeOf]).
     * Empty on every other row and on a commit that changed no people; a
     * commit row with both lists empty is never drawn. Names are resolved
     * when drawn, never stored.
     */
    val membershipJoined: List<MemberIdentity> = emptyList(),
    /** The same, for the person keys that left. */
    val membershipLeft: List<MemberIdentity> = emptyList(),
    /** ADR-0092 — the poll, checklist, location, contact card or article this
     *  row carries; `null` for an ordinary message. */
    val content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    /** ADR-0092 — the ops, ballots and ring the row's content collected. */
    val contentExtras: ContentExtras = ContentExtras(),
) {
    /** Whether this is a read-only row of an older segment. */
    val isHistory: Boolean get() = historyOf != null

    /** ADR-0082 — the highest op that touched this message (ADR-0023): its
     *  own id until an edit or delete lands. */
    val effectiveOpId: Long get() = maxOf(messageId, lastOpId)

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
    /** The DS message id of the add that gave it (orders chips); 0 when
     *  unknown. */
    val at: Long = 0L,
)

public enum class Direction { Outgoing, Incoming, System }

/**
 * An identity no row of [messages] holds: negative, and below every [ChatMessage.rowId]
 * there, sent rows included. A sent row keeps its local id as its identity, so counting
 * below `messageId` hands a call row the id of the message before it, and two rows with
 * one key bring the timeline down.
 */
internal fun nextLocalRowId(messages: List<ChatMessage>): Long =
    minOf(0L, messages.minOfOrNull { it.rowId } ?: 0L) - 1

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

    /** Whether [knockCursor] was raised to the saved position yet. */
    @Volatile
    var knockCursorLoaded: Boolean = false

    @Volatile
    var lastKnockPollMs: Long = 0L

    /** ADR-0087 section 5 — the knock cursor stopped on a full pending set:
     *  more requests wait on the server (the `64+` heading). */
    @Volatile
    var knockQueueFull: Boolean = false

    /** The newest knock row id this device read (`Reject all`'s `through`). */
    @Volatile
    var lastSeenKnockId: Long = 0L

    /** reactions-and-emoji section 10 — the core's reaction fold (a test may
     *  swap it; the host JVM has no native library). */
    var reactionFold: ReactionFold = NativeReactionFold

    /** The stored author identity of message [messageId], or `null`. */
    fun messageAuthorKey(messageId: Long): MemberIdentity? =
        synchronized(messages) { messages.firstOrNull { it.messageId == messageId }?.authorKey }

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
    fun nextLocalId(): Long = nextLocalRowId(messages)

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
        /** ADR-0087 section 14 — a channel invitation card, likewise. */
        channelInvite: ByteArray? = null,
        content: uniffi.lakko_ffi_uniffi.MessageContent? = null,
    ) {
        messages.add(
            ChatMessage(
                content = content,
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
                channelInvite = channelInvite,
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

    /** ADR-0073 R.7 case 1 — a held row whose message went out in the
     *  conversation's successor leaves this segment. */
    fun removeLocal(localId: Long) {
        messages.removeAll { it.localId == localId && it.ack != Ack.SENT }
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
        /** Reactions: add (`true`) or take off. */
        reactionAdd: Boolean = true,
        /** The op's DS message id, `null` when the publish returned none or
         *  the op is local-only. Reactions order by it; ADR-0082 edit / delete
         *  stamp it as [ChatMessage.lastOpId]. */
        opId: Long? = null,
    ) {
        applyOpInternal(
            op = op,
            target = target,
            text = text,
            emoji = emoji,
            sender = if (op == MessageOp.Reaction) ownKey ?: OWN_REACTION_SENTINEL else ownKey,
            reactionAdd = reactionAdd,
            opId = opId,
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
        /** ADR-0091 phase 2 — the group's knock key from (sender key, envelope). */
        knockKeySink: ((MemberIdentity?, uniffi.lakko_ffi_uniffi.FfiKnockKey) -> Unit)? = null,
        scopedSink: ((MemberIdentity?, me.lakko.app.reach.ScopedEnvelopeKind, ByteArray) -> Unit)? = null,
        groupSettingSink: ((MemberIdentity?, GroupSettingKind, ByteArray) -> Unit)? = null,
        groupClosedSink: ((Long, MemberIdentity?, uniffi.lakko_ffi_uniffi.FfiGroupClosed) -> Unit)? = null,
        /** `docs/ux/who-can-find-me.md` section 6.4 — a contact request sent
         *  inside the group: (sender key, envelope, DS message id, created at). */
        groupContactRequestSink: ((MemberIdentity?, ByteArray, Long, String) -> Unit)? = null,
        /** ADR-0073 R.7 case 2 / UX 16.5 — the batch changed which devices the
         *  group holds and not which PEOPLE: its commit rows are not drawn
         *  ([deviceSwapAcrossCommits]). */
        suppressCommitRows: Boolean = false,
        /** `docs/ux/second-device.md` section 4 — the people this batch
         *  added and removed in a GROUP ([membershipChangeOf]), recorded on
         *  the batch's LAST commit row; `null` records nothing. */
        membershipChange: MembershipChange? = null,
        /** ADR-0073 R.7 case 1 — whether a received message is a RE-SEND
         *  whose original this device already holds ([resendVerdict]): such a
         *  row is dropped before storage, so it is shown, counted and
         *  notified once, where it was. */
        heldResend: ((IncomingChatEvent.Application) -> Boolean)? = null,
    ) {
        // ADR-0050 — a message this PERSON sent from another device arrives as an
        // incoming row (another leaf sealed it) but is ours: it renders on our
        // side, exactly as if this device had sent it.
        val ownPersonKey by lazy { ownIdentity(group) }
        // The batch's last commit row carries the membership change.
        val membershipRowIndex = membershipRowIndex(events, membershipChange, suppressCommitRows)
        for ((eventIndex, event) in events.withIndex()) {
            when (event) {
                is IncomingChatEvent.Application -> if (heldResend?.invoke(event) != true &&
                    contentAdmissible(event)
                ) addUnlessFromBundle(
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
                        // ADR-0087 section 14 — a channel invitation card.
                        channelInvite = event.channelInvite,
                        // ADR-0092 — a content message and, for an anonymous
                        // poll, its ring version 0 as this device derived it.
                        content = event.content,
                        contentExtras = ContentExtras(ring = ringForIncomingPoll(event)),
                    ),
                )
                is IncomingChatEvent.ContentOp -> applyContentOp(event)
                is IncomingChatEvent.OpenItems -> acceptOpenItems(event)
                is IncomingChatEvent.PollBallotRow -> attachBallot(event.messageId, event.createdAt, event.row)
                is IncomingChatEvent.Op -> applyOpInternal(
                    op = event.op,
                    target = event.target,
                    text = event.text,
                    emoji = event.emoji,
                    sender = event.senderKey,
                    reactionAdd = event.reactionAdd,
                    opId = event.messageId,
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
                is IncomingChatEvent.System -> {
                    event.rosterAccountKeys?.let(::extendOpenPollRings)
                    if (!(suppressCommitRows && event.systemKind == SystemKind.Commit)) messages.add(
                    ChatMessage(
                        messageId = event.messageId,
                        direction = Direction.System,
                        text = systemRowText(event.systemKind),
                        createdAt = event.createdAt,
                        membershipJoined = if (eventIndex == membershipRowIndex) {
                            membershipChange?.joined.orEmpty()
                        } else {
                            emptyList()
                        },
                        membershipLeft = if (eventIndex == membershipRowIndex) {
                            membershipChange?.left.orEmpty()
                        } else {
                            emptyList()
                        },
                    ),
                )
                }
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
                    // ADR-0073 R.6 — a successor claim. Never a timeline row and
                    // never drawn (R.0); the sink runs the receive rule and
                    // writes the claim state only on its verdict.
                    val closed = event.closed
                    if (closed != null) groupClosedSink?.invoke(event.messageId, event.senderKey, closed)
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
                is IncomingChatEvent.KnockKey -> {
                    // ADR-0091 phase 2 — the group's knock key. Not a timeline
                    // row; the sink keeps it from the owner or an admin only.
                    event.knock?.let { knockKeySink?.invoke(event.senderKey, it) }
                }
                is IncomingChatEvent.GroupSetting -> {
                    // ADR-0087 — a group setting. Not a timeline row; the sink
                    // decodes it and runs its gate under the per-group lock.
                    groupSettingSink?.invoke(event.senderKey, event.kind, event.bytes)
                }
                is IncomingChatEvent.ScopedDiscovery -> {
                    // ADR-0060 — a scoped-discovery service envelope. Not a
                    // timeline row; the sink keeps what this device acts on
                    // and drops the rest.
                    scopedSink?.invoke(event.senderKey, event.kind, event.bytes)
                }
                is IncomingChatEvent.GroupContactRequest -> {
                    // Section 6.4 — never a timeline row; the sink opens it
                    // with this account's key or drops it.
                    groupContactRequestSink?.invoke(event.senderKey, event.envelope, event.messageId, event.createdAt)
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

    // ------------------------------------------------------------ ADR-0092

    /** Whether this device's account is the owner or an admin of the live role table. */
    fun ownIsPrivileged(): Boolean = privilegedNow(
        try {
            ownIdentity(group)
        } catch (_: Throwable) {
            null
        },
    )

    /** Whether the account [sender] is the owner or an admin of the live role table. */
    private fun privilegedNow(sender: MemberIdentity?): Boolean {
        if (sender == null) return false
        return try {
            val roles = group.groupRoles() ?: return false
            outboxB64(roles.owner) == sender || roles.admins.any { outboxB64(it) == sender }
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * ADR-0092 sections 3.1 and 5 — a poll or checklist is kept only when its
     * author signature verifies against the account the MLS sender resolved
     * to at receipt (C-68); an anonymous poll is refused in a one-to-one. Any
     * other row is admissible.
     */
    private fun contentAdmissible(event: IncomingChatEvent.Application): Boolean {
        val content = event.content ?: return true
        val author = identityBytes(event.senderKey)
        return try {
            when (content) {
                is uniffi.lakko_ffi_uniffi.MessageContent.Poll -> {
                    if (author == null) return false
                    if (content.poll.body.anonymous != null &&
                        group.groupKind() == uniffi.lakko_ffi_uniffi.GroupKind.ONE_TO_ONE
                    ) {
                        return false
                    }
                    uniffi.lakko_ffi_uniffi.verifyPoll(content.poll, groupId, author)
                    true
                }
                is uniffi.lakko_ffi_uniffi.MessageContent.Checklist -> {
                    if (author == null) return false
                    uniffi.lakko_ffi_uniffi.verifyChecklist(content.checklist, groupId, author)
                    true
                }
                else -> true
            }
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * ADR-0092 section 3.4 — version 0 of an anonymous poll's ring: the roster
     * at receipt, kept only when its hash is the one the author signed. A
     * mismatch leaves the poll unverifiable (no ring), fail-closed.
     */
    private fun ringForIncomingPoll(event: IncomingChatEvent.Application): ByteArray? {
        val anon = (event.content as? uniffi.lakko_ffi_uniffi.MessageContent.Poll)?.poll?.body?.anonymous
            ?: return null
        val keys = event.rosterAccountKeys ?: return null
        return ringBytesIfMatches(keys, anon.ring0Hash)
    }

    /** Commit order — every open anonymous poll appends the accounts a commit added. */
    private fun extendOpenPollRings(rosterKeys: List<ByteArray>) {
        if (rosterKeys.isEmpty()) return
        val now = System.currentTimeMillis()
        synchronized(messages) {
            for ((i, m) in messages.withIndex()) {
                val ringBytes = m.contentExtras.ring ?: continue
                if (!m.content.isAnonymousPoll()) continue
                val view = pollViewOf(m, groupId, null, now)
                if (view?.closed == true) continue
                val next = extendRing(ringBytes, rosterKeys) ?: continue
                messages[i] = m.copy(contentExtras = m.contentExtras.copy(ring = next))
            }
        }
    }

    /** ADR-0092 — fold an op into its target row. Unknown target: dropped. */
    fun applyContentOp(event: IncomingChatEvent.ContentOp) {
        val sender = event.senderKey ?: return
        val record = ContentOpRecord(
            kind = event.op,
            opId = event.messageId,
            createdAtMs = isoToMs(event.createdAt),
            sender = sender,
            privileged = privilegedNow(sender),
            choices = event.choices,
            item = event.item,
            done = event.done,
            text = event.text,
            content = event.content,
            attachments = event.attachments,
        )
        addContentOp(event.target, record)
    }

    /** Add [op] to the row [target] (own ops included), once per op id. */
    fun addContentOp(target: Long, op: ContentOpRecord) {
        synchronized(messages) {
            val idx = messages.indexOfFirst { it.messageId == target }
            if (idx < 0) return
            val current = messages[idx]
            if (current.content == null || current.deleted) return
            if (current.contentExtras.ops.any { it.opId == op.opId && it.kind == op.kind }) return
            // An edit replaces the whole checklist or article: kept only from
            // the author, with a valid signature for a checklist.
            if (op.kind == ContentOpKind.EditChecklist || op.kind == ContentOpKind.EditArticle) {
                if (op.sender != current.authorKey) return
                val signed = (op.content as? uniffi.lakko_ffi_uniffi.MessageContent.Checklist)?.checklist
                if (op.kind == ContentOpKind.EditChecklist) {
                    val author = identityBytes(op.sender) ?: return
                    try {
                        uniffi.lakko_ffi_uniffi.verifyChecklist(signed ?: return, groupId, author)
                    } catch (_: Throwable) {
                        return
                    }
                }
            }
            messages[idx] = current.copy(
                contentExtras = current.contentExtras.copy(ops = current.contentExtras.ops + op),
                lastOpId = maxOf(current.lastOpId, op.opId),
            )
        }
    }

    /**
     * ADR-0092 section 3.6 — a ballot row is kept on the open anonymous poll
     * whose keys open it (the poll id is inside the sealed ballot).
     */
    fun attachBallot(messageId: Long, createdAt: String, row: ByteArray) {
        synchronized(messages) {
            for ((i, m) in messages.withIndex()) {
                val anon = (m.content as? uniffi.lakko_ffi_uniffi.MessageContent.Poll)?.poll?.body ?: continue
                val secret = anon.anonymous?.secret ?: continue
                val opened = try {
                    uniffi.lakko_ffi_uniffi.PollBallotKeys(groupId, anon.id, secret).use { keys ->
                        uniffi.lakko_ffi_uniffi.openPollBallotRow(keys, row)
                        true
                    }
                } catch (_: Throwable) {
                    false
                }
                if (!opened) continue
                if (m.contentExtras.ballots.any { it.messageId == messageId }) return
                messages[i] = m.copy(
                    contentExtras = m.contentExtras.copy(
                        ballots = m.contentExtras.ballots + BallotRecord(messageId, isoToMs(createdAt), row),
                    ),
                )
                return
            }
        }
    }

    /** ADR-0092 section 3.9 — the account that admitted this device into this group. */
    private val admittingAccount: ByteArray?
        get() = AdmittingAccounts.current(me.lakko.app.ds.DsClient.pseudonymHex(groupId))

    /** ADR-0092 section 3.9 — open-items rows received so far, assembled when whole. */
    private val openItemsRows = mutableListOf<uniffi.lakko_ffi_uniffi.ReceivedOpenItemsRow>()

    /**
     * ADR-0092 section 3.9 — a newcomer's side of the hand-over: keep a row
     * only from an owner or admin (the admitting account is not tracked on
     * this client, so a self-service admission by a plain member is not
     * trusted), assemble the rows into whole items, adopt each and add it to
     * the conversation at its place; old ballot rows go onto their poll.
     */
    fun acceptOpenItems(event: IncomingChatEvent.OpenItems) {
        val sender = identityBytes(event.senderKey) ?: return
        val trusted = try {
            uniffi.lakko_ffi_uniffi.openItemsSenderTrusted(sender, admittingAccount, privilegedNow(event.senderKey))
        } catch (_: Throwable) {
            false
        }
        if (!trusted) return
        val ballots = try {
            if (uniffi.lakko_ffi_uniffi.isPollBallotsEnvelope(event.envelope)) {
                uniffi.lakko_ffi_uniffi.decodePollBallotsEnvelope(event.envelope)
            } else {
                null
            }
        } catch (_: Throwable) {
            null
        }
        if (ballots != null) {
            val createdAt = isoToMs(event.createdAt)
            for ((i, row) in ballots.rows.withIndex()) {
                attachBallotTo(ballots.pollRef, ballots.pollRef + 1 + i, createdAt, row)
            }
            return
        }
        synchronized(messages) {
            openItemsRows.add(uniffi.lakko_ffi_uniffi.ReceivedOpenItemsRow(sender, event.envelope))
            val held = messages.filter { it.content != null }.map { it.messageId }
            val assembled = try {
                uniffi.lakko_ffi_uniffi.assembleOpenItems(openItemsRows.toList(), held)
            } catch (_: Throwable) {
                emptyList()
            }
            for (item in assembled) {
                val adopted = try {
                    uniffi.lakko_ffi_uniffi.adoptOpenItem(groupId, item)
                } catch (_: Throwable) {
                    null
                } finally {
                    item.close()
                }
                when (adopted) {
                    is uniffi.lakko_ffi_uniffi.AdoptedOpenItem.Poll -> insertAdopted(
                        adopted.pollRef,
                        adopted.author,
                        uniffi.lakko_ffi_uniffi.MessageContent.Poll(adopted.poll),
                        ContentExtras(
                            ops = adopted.votes.map {
                                ContentOpRecord(ContentOpKind.PollVote, it.messageId, it.createdAtMs, outboxB64(it.voter), false, choices = it.choices)
                            },
                            ring = adopted.ring?.encode(),
                        ),
                    )
                    is uniffi.lakko_ffi_uniffi.AdoptedOpenItem.Checklist -> insertAdopted(
                        adopted.pollRef,
                        adopted.author,
                        uniffi.lakko_ffi_uniffi.MessageContent.Checklist(adopted.checklist),
                        ContentExtras(
                            ops = adopted.adds.map {
                                ContentOpRecord(ContentOpKind.ChecklistAdd, it.messageId, 0, outboxB64(it.adder), it.privileged, text = it.text)
                            } + adopted.ticks.map {
                                ContentOpRecord(ContentOpKind.ChecklistTick, it.messageId, 0, outboxB64(it.ticker), it.privileged, item = it.item, done = it.done)
                            },
                        ),
                    )
                    null -> Unit
                }
            }
            // Rows of adopted items stay until the app stops (assembleOpenItems
            // skips held items); bounded by what one hand-over carries.
            if (openItemsRows.size > MAX_HELD_OPEN_ITEMS_ROWS) openItemsRows.subList(0, openItemsRows.size - MAX_HELD_OPEN_ITEMS_ROWS).clear()
        }
    }

    private fun insertAdopted(pollRef: Long, author: ByteArray, content: uniffi.lakko_ffi_uniffi.MessageContent, extras: ContentExtras) {
        if (messages.any { it.messageId == pollRef }) return
        val own = try {
            ownIdentity(group)
        } catch (_: Throwable) {
            null
        }
        val authorKey = outboxB64(author)
        val row = ChatMessage(
            messageId = pollRef,
            direction = if (authorKey == own) Direction.Outgoing else Direction.Incoming,
            text = "",
            createdAt = java.time.Instant.EPOCH.toString(),
            authorKey = authorKey,
            content = content,
            contentExtras = extras,
        )
        val at = messages.indexOfFirst { it.messageId > pollRef && it.messageId > 0 }
        if (at < 0) messages.add(row) else messages.add(at, row)
    }

    private fun attachBallotTo(pollRef: Long, messageId: Long, createdAtMs: Long, row: ByteArray) {
        synchronized(messages) {
            val idx = messages.indexOfFirst { it.messageId == pollRef }
            if (idx < 0) return
            val m = messages[idx]
            if (m.contentExtras.ballots.any { it.row.contentEquals(row) }) return
            messages[idx] = m.copy(
                contentExtras = m.contentExtras.copy(ballots = m.contentExtras.ballots + BallotRecord(messageId, createdAtMs, row)),
            )
        }
    }

    /** Replace a row's own anonymous vote state (UX 7.5). */
    fun setOwnBallot(target: Long, ballot: OwnBallot?) {
        synchronized(messages) {
            val idx = messages.indexOfFirst { it.messageId == target }
            if (idx < 0) return
            val m = messages[idx]
            messages[idx] = m.copy(contentExtras = m.contentExtras.copy(ownBallot = ballot))
        }
    }

    /** Record the ring of an own anonymous poll once it has its DS id. */
    fun setRing(localOrMessageId: Long, ring: ByteArray?) {
        synchronized(messages) {
            val idx = messages.indexOfFirst { it.messageId == localOrMessageId || it.localId == localOrMessageId }
            if (idx < 0) return
            val m = messages[idx]
            messages[idx] = m.copy(contentExtras = m.contentExtras.copy(ring = ring))
        }
    }

    fun messageById(messageId: Long): ChatMessage? =
        synchronized(messages) { messages.firstOrNull { it.messageId == messageId } }

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
        reactionAdd: Boolean = true,
        /** The op's own DS id (`null` = unknown): orders reactions and, for
         *  ADR-0082, marks a stored translation of the edited message stale. */
        opId: Long? = null,
    ) {
        synchronized(messages) {
            val idx = messages.indexOfFirst { it.messageId == target }
            if (idx < 0) return // unknown target — drop
            val current = messages[idx]
            when (op) {
                MessageOp.Edit -> {
                    if (!editDeleteAuthorized(current.authorKey, sender)) return
                    if (current.deleted) return // a deleted message stays deleted
                    messages[idx] = current.copy(
                        text = text,
                        edited = true,
                        lastOpId = nextLastOpId(current, opId),
                    )
                }
                MessageOp.Delete -> {
                    if (!editDeleteAuthorized(current.authorKey, sender)) return
                    messages[idx] = current.copy(
                        text = "",
                        attachments = emptyList(),
                        deleted = true,
                        lastOpId = nextLastOpId(current, opId),
                    )
                    // ADR-0082 section 5 — its translation and transcript go too.
                    me.lakko.app.translation.TranslationCenter.messageDeleted(
                        AttachmentBridge.hex(groupId),
                        target,
                    )
                }
                MessageOp.Reaction -> {
                    // reactions-and-emoji section 10 — the op is kept and the
                    // held set is the core's fold over every op, so every
                    // member draws the same chips whatever order the rows
                    // were applied in.
                    val e = emoji ?: return
                    if (sender == null) return
                    val ops = appendReactionOp(
                        current.reactionOps,
                        ReactionOpRecord(
                            person = sender,
                            emoji = e,
                            add = reactionAdd,
                            at = opId ?: localOpId(current.reactionOps),
                        ),
                    )
                    val held = reactionFold.fold(ops) ?: current.reactions
                    messages[idx] = current.copy(reactions = held, reactionOps = ops)
                    // Section 12 — someone else's new reaction on one of this
                    // person's own messages is an unseen reaction.
                    val author = current.authorKey
                    if (reactionAdd && author != null && sender != author &&
                        held.any { it.reactor == sender && it.emoji == e }
                    ) {
                        val own = try {
                            ownIdentity(group)
                        } catch (_: Throwable) {
                            null
                        }
                        if (own != null && own == author) {
                            UnseenReactions.mark(groupId.joinToString("") { "%02x".format(it) }, target)
                        }
                    }
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
        syncedThrough = syncedThroughMessageId
            ?.let { mapOf(DsClient.pseudonymHex(groupId).lowercase() to it) }
            .orEmpty(),
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

    /** ADR-0087 section 9 — the record stays a little longer than what it
     *  showed: a log persist of another group writes this one from here. */
    fun wipeTimeline() {
        messages.clear()
        seedFeedSeen(emptySet(), null)
        synchronized(presenceLock) {
            typingByMember = emptyMap()
            readUpToByMember = emptyMap()
        }
    }

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

    /** "History is not news" (`docs/ux/second-device.md` section 2) — the
     *  highest id a pairing bundle carried for this group; `null` when none
     *  did. Moves up only. */
    @Volatile
    var syncedThroughMessageId: Long? = null
        private set

    fun raiseSyncedThrough(floor: Long?) {
        syncedThroughMessageId = higherFloor(syncedThroughMessageId, floor)
    }

    /**
     * Take in what the log held for this group: the rows of a history bundle
     * and its floor. The floor and the rows move under the monitor
     * [addUnlessFromBundle] takes, so a polled row is never added beside the
     * same row of a bundle landing at that moment.
     */
    fun adoptHistory(slot: PersistedGroup) {
        val rows = slot.messages.mapNotNull { it.toChatMessage() }
        synchronized(messages) {
            raiseSyncedThrough(slot.syncedThroughMessageId)
            val merged = mergeHistoryRows(messages.toList(), rows)
            messages.clear()
            messages.addAll(merged)
        }
        if (slot.feedSeen.isNotEmpty() || slot.feedSeenThrough != null) {
            seedFeedSeen(feedSeen + slot.feedSeen, higherFloor(feedSeenThrough, slot.feedSeenThrough))
        }
    }

    /** A polled message, unless the history bundle already brought it. */
    private fun addUnlessFromBundle(row: ChatMessage) {
        synchronized(messages) {
            if (!heldFromBundle(syncedThroughMessageId, row.messageId, messages)) messages.add(row)
        }
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
            messages.add(
                ChatMessage(
                    messageId = nextLocalId(),
                    direction = Direction.System,
                    text = me.lakko.app.calls.callRowText(entry),
                    createdAt = createdAt,
                    call = entry,
                ),
            )
        }
    }

    /**
     * ADR-0073 phase 2 / UX 16.3 — append a local system row ("{name} reset
     * their account keys. Their old devices were removed from this group.")
     * at the point the eviction landed. Local like a call row (a negative id
     * no DS row can take, no author), rendered in the interface language of
     * the moment it is written, as a call row is.
     */
    fun appendOwnCommitRow(messageId: Long, createdAt: String, change: MembershipChange): Boolean =
        synchronized(messages) {
            val at = ownCommitRowInsertIndex(messages, messageId) ?: return@synchronized false
            messages.add(at, ownCommitRow(messageId, createdAt, change))
            true
        }

    fun appendLocalSystemRow(text: me.lakko.app.l10n.UiText) {
        synchronized(messages) {
            messages.add(
                ChatMessage(
                    messageId = nextLocalId(),
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
        SystemKind.Commit -> COMMIT_ROW_TEXT
        SystemKind.ExternalJoinProposal -> EXTERNAL_JOIN_ROW_TEXT
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

/**
 * ADR-0082 — the [ChatMessage.lastOpId] after an op with DS id [opId] (`null`
 * or 0 when the caller has none). A real id is used as it is (idempotent when
 * the same op is applied twice); without one the value still rises, so the
 * edit makes a stored result stale.
 */
internal fun nextLastOpId(current: ChatMessage, opId: Long?): Long =
    if (opId != null && opId > 0L) maxOf(opId, current.lastOpId) else current.effectiveOpId + 1

/** How many long polls a newly joined group gets of its own, and how long each
 *  waits: its first rows are this device's own profile, then the other
 *  person's. Together about one sweep. Chosen, not measured. */
private const val JOINED_GROUP_POLLS: Int = 3
private const val JOINED_GROUP_POLL_WAIT_SECS: Int = 10

/** ADR-0087 section 3 — renew a handle with fewer than 90 days left. */
private const val HANDLE_RENEW_BEFORE_MS: Long = 90L * 24 * 60 * 60 * 1000

/** Section 3 — the lease a write grants when its answer named none. */
private const val HANDLE_LEASE_MS: Long = 180L * 24 * 60 * 60 * 1000

/** Section 3 — at most this many renewals in one foreground session. */
private const val HANDLE_RENEWALS_PER_SESSION: Int = 20

/** ADR-0087 4.5.2 — the device is no longer owner or admin: nothing sent. */
public class GroupPhotoNotAllowed : IllegalStateException("not allowed to change the group photo")

/** ADR-0087 4.5.2 — uploaded, not sent yet (offline): kept owed. */
public class GroupPhotoOwed : IllegalStateException("group photo owed")

/** ADR-0087 section 3 — wait before checking an unverified @name again. */
private const val HANDLE_VERIFY_RETRY_MS: Long = 60_000L

/** ADR-0060 section 1 (2026-11-06) — [grants] without the `contact` grant
 *  when [withhold] (a removed person's one-to-one), else unchanged. */
internal fun withoutContactGrant(
    grants: List<uniffi.lakko_ffi_uniffi.FfiReachGrant>,
    withhold: Boolean,
): List<uniffi.lakko_ffi_uniffi.FfiReachGrant> =
    if (withhold) grants.filterNot { it.`class` == uniffi.lakko_ffi_uniffi.FfiReachClass.CONTACT } else grants
