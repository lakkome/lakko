// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import android.content.Context
import android.util.Log
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicReference
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.ds.DsReachAuth
import me.lakko.app.ds.DsReachKeyAdd
import me.lakko.app.ds.FfiDsJournalSigner
import me.lakko.app.ds.reachSigner
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachGrant
import uniffi.lakko_ffi_uniffi.FfiReachState
import uniffi.lakko_ffi_uniffi.FfiScopeId
import uniffi.lakko_ffi_uniffi.FfiScopeKind
import uniffi.lakko_ffi_uniffi.FfiVisibilityMode
import uniffi.lakko_ffi_uniffi.FfiVisibilitySetting
import uniffi.lakko_ffi_uniffi.Identity

/** Whether the setting the person chose is in effect on the server
 *  (`docs/ux/who-can-find-me.md` sections 8 and 9). */
public enum class ReachApplyStatus {
    /** What the screen shows is what the server holds. */
    APPLIED,

    /** An update is on its way. */
    APPLYING,

    /** The device could not reach the server; the choice applies later. */
    OFFLINE,

    /** The server refused, or the session is gone. */
    FAILED,
}

/**
 * ADR-0060 — the owner's reach keys and the grants other people handed this
 * device, bound to the signed-in account.
 *
 * - Derives every one of the owner's keys from the account seed and a version
 *   counter (nothing secret of the owner's is stored).
 * - Keeps the owner's mailbox holding exactly [desiredReachKeys]: one
 *   `PUT /ds/mailbox/{hex}/reach` per change, signed by the identity proof.
 * - Keeps the grants received, and picks the one a write into someone's
 *   mailbox is signed with ([reachAuthFor]).
 * - Holds the "who can find me" setting.
 *
 * The setting and the version counters are shared with the account's other
 * devices (ADR-0060 §7): a local change goes out through [deviceSyncPublisher]
 * as a DeviceGroup service envelope, and one received from a sibling is kept
 * by [adoptDeviceSync] when strictly newer. A device admitting a new one sends
 * it [deviceSyncEnvelopes], each with its original stamp.
 */
public class ReachManager internal constructor(
    private val store: ReachDocStore,
    private val dsFactory: () -> DsClient = { DsClient() },
    private val ownProfiles: OwnProfileAccess? = null,
) {
    /** The own-profile store (ADR-0048 section 4a), keyed by the raw account
     *  id: the own name travels between the account's devices beside the
     *  setting. */
    public interface OwnProfileAccess {
        /** The stored own profile of [accountId], or `null`. */
        public fun load(accountId: ByteArray): me.lakko.app.profile.OwnProfile?

        /** Apply [transform] to the stored profile of [accountId] as one
         *  read-modify-write; a `null` result stores nothing. Returns whether
         *  something was stored. */
        public fun update(
            accountId: ByteArray,
            transform: (me.lakko.app.profile.OwnProfile?) -> me.lakko.app.profile.OwnProfile?,
        ): Boolean
    }

    private class Binding(
        val accountIdB64: String,
        /** The account identity's 32-byte Ed25519 seed (`reachSeed` input). */
        val accountSeed: ByteArray,
        /** The account identity's signature public key. */
        val accountKey: ByteArray,
        /** The account mailbox pseudonym. */
        val mailbox: ByteArray,
        /** The identity proof signer of that mailbox. */
        val mailboxSigner: DsJournalSigner,
    )

    private val binding = AtomicReference<Binding?>(null)
    private val lock = Mutex()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val random = SecureRandom()

    @Volatile
    private var doc: ReachDoc = ReachDoc()

    private val mutableVisibility = MutableStateFlow<FfiVisibilitySetting?>(null)

    /** The setting as the person chose it, `null` before a session binds. */
    public val visibility: StateFlow<FfiVisibilitySetting?> = mutableVisibility.asStateFlow()

    private val mutableStatus = MutableStateFlow(ReachApplyStatus.APPLIED)

    /** Whether [visibility] is in effect on the server. */
    public val applyStatus: StateFlow<ReachApplyStatus> = mutableStatus.asStateFlow()

    /** Called after a setting change landed (profiles republished, Everyone
     *  published or withdrawn). Installed by the chat repository. */
    @Volatile
    public var onVisibilityChanged: (suspend (FfiVisibilitySetting) -> Unit)? = null

    /** ADR-0048 section 4a — the own name was adopted from another of the
     *  account's devices: what follows the name here (the Everyone
     *  publication) is refreshed. Never a group republish. */
    public var onOwnNameAdopted: (suspend () -> Unit)? = null

    /** Sends one `lakko-contacts` envelope to the account's other devices over
     *  the DeviceGroup (ADR-0060 §7). Installed by the DeviceGroup sync
     *  runtime; `null` while none runs, when a change stays on this device
     *  until [deviceSyncEnvelopes] is next sent. */
    @Volatile
    public var deviceSyncPublisher: ((ByteArray) -> Unit)? = null

    /** Envelopes a sibling sent before a session bound here, applied on
     *  [bind] (the newest of each kind wins there by the usual rule). */
    private val pendingDeviceSync = java.util.concurrent.ConcurrentLinkedQueue<ByteArray>()

    // -----------------------------------------------------------------
    // Session
    // -----------------------------------------------------------------

    /** Bind to the signed-in account and reconcile its mailbox. Idempotent. */
    public fun bind(accountIdB64: String, account: Identity) {
        val current = binding.get()
        if (current != null && current.accountIdB64 == accountIdB64) {
            launchSync()
            return
        }
        val ffiSigner = uniffi.lakko_ffi_uniffi.dsMailboxSigner(account)
        val bound = Binding(
            accountIdB64 = accountIdB64,
            accountSeed = account.ed25519Seed(),
            accountKey = account.signaturePublicKey(),
            mailbox = ffiSigner.mailboxPseudonym(),
            mailboxSigner = FfiDsJournalSigner(ffiSigner),
        )
        binding.set(bound)
        doc = store.load(accountIdB64)
        mutableVisibility.value = decodeVisibility(doc.visibilityHex)
        launchSync()
        while (true) {
            val pending = pendingDeviceSync.poll() ?: break
            adoptDeviceSync(pending)
        }
    }

    /** Forget the session (sign-out, forget device). The stored document stays
     *  with the account; forget device deletes it through
     *  [me.lakko.app.account.AccountLocalWipe]. */
    public fun clear() {
        val old = binding.getAndSet(null)
        old?.accountSeed?.fill(0)
        pendingDeviceSync.clear()
        doc = ReachDoc()
        mutableVisibility.value = null
        mutableStatus.value = ReachApplyStatus.APPLIED
    }

    /** The bound account's signature public key, or `null`. */
    public fun ownAccountKey(): ByteArray? = binding.get()?.accountKey?.copyOf()

    /**
     * The account bound at one moment, for work that is handed something FOR
     * that account and runs later (a launched intake): it passes this back
     * and the manager does nothing when another account, or none, is bound by
     * then. A sign-out and a new sign-in to the SAME account is a new one.
     */
    public class BoundAccount internal constructor(internal val token: Any)

    /** The account bound now, or `null`. */
    public fun boundAccount(): BoundAccount? = binding.get()?.let(::BoundAccount)

    /** Whether [account] is still the bound one. */
    public fun isBound(account: BoundAccount): Boolean = binding.get() === account.token

    // -----------------------------------------------------------------
    // The owner's keys
    // -----------------------------------------------------------------

    /** The owner's current grant for [kind] over [scope] (empty for
     *  `contact` / `fof`), or `null` before a session binds. SECRET. */
    public fun grant(kind: ReachKind, scope: ByteArray = ByteArray(0)): FfiReachGrant? {
        val b = binding.get() ?: return null
        val slot = ReachSlot(kind, if (kind.scopeless) "" else reachHex(scope))
        return derive(b, slot, versionOf(doc, slot))
    }

    /**
     * A fresh `reply` grant for a request going to [toKey] (ADR-0060 §6),
     * registered with its seven-day expiry BEFORE this returns: a request
     * whose reply key is not on the server would be answered into a closed
     * mailbox. Throws when the registration does not land.
     */
    public suspend fun mintReply(toKey: ByteArray?, nowMs: Long = System.currentTimeMillis()): FfiReachGrant {
        val b = binding.get() ?: throw IllegalStateException("no session")
        val scope = ByteArray(REPLY_SCOPE_LEN).also(random::nextBytes)
        val scopeHex = reachHex(scope)
        lock.withLock {
            if (binding.get() !== b) throw IllegalStateException("no session")
            doc = doc.copy(
                replies = doc.replies + StoredReply(scopeHex, replyExpiry(nowMs), toKey?.let(::reachHex)),
            )
            store.save(b.accountIdB64, doc)
            if (!syncLocked(b, nowMs)) throw IllegalStateException("reply key not registered")
        }
        return derive(b, ReachSlot(ReachKind.REPLY, scopeHex), 0)
    }

    /** An answer arrived from [fromKey] (their ContactAck, a Welcome from
     *  them): retire every reply key waiting on them. */
    public fun retireRepliesTo(fromKey: ByteArray) {
        val b = binding.get() ?: return
        val keyHex = reachHex(fromKey)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                val kept = doc.replies.filterNot { it.toKeyHex == keyHex }
                if (kept.size == doc.replies.size) return@withLock
                doc = doc.copy(replies = kept)
                store.save(b.accountIdB64, doc)
                syncLocked(b, System.currentTimeMillis())
            }
        }
    }

    /**
     * The `link` grant for a new invite link [linkId] (`generateLinkId()`),
     * registered BEFORE this returns (with the link's expiry, `null` = never):
     * a link shared before its key is on the server would be refused.
     */
    public suspend fun mintLink(linkId: ByteArray, expiresAtMs: Long?): FfiReachGrant {
        val b = binding.get() ?: throw IllegalStateException("no session")
        val linkHex = reachHex(linkId)
        lock.withLock {
            if (binding.get() !== b) throw IllegalStateException("no session")
            doc = doc.copy(links = doc.links.filterNot { it.linkIdHex == linkHex } + StoredLink(linkHex, expiresAtMs))
            store.save(b.accountIdB64, doc)
            if (!syncLocked(b, System.currentTimeMillis())) throw IllegalStateException("link key not registered")
        }
        return derive(b, ReachSlot(ReachKind.LINK, linkHex), versionOf(doc, ReachSlot(ReachKind.LINK, linkHex)))
    }

    /** Remember the ledger id [linkId] was uploaded under, so a revoke by
     *  ledger id finds its key. */
    public fun noteLinkBlob(linkId: ByteArray, blobId: ByteArray) {
        val b = binding.get() ?: return
        val linkHex = reachHex(linkId)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(
                    links = doc.links.map { if (it.linkIdHex == linkHex) it.copy(blobIdHex = reachHex(blobId)) else it },
                )
                store.save(b.accountIdB64, doc)
            }
        }
    }

    /** A link uploaded under [blobId] was revoked: its key comes off. */
    public suspend fun retireLinkByBlob(blobId: ByteArray) {
        val blobHex = reachHex(blobId)
        val link = doc.links.firstOrNull { it.blobIdHex == blobHex } ?: return
        reachUnhex(link.linkIdHex)?.let { retireLink(it) }
    }

    /** A link was revoked or expired: its key comes off the mailbox. */
    public suspend fun retireLink(linkId: ByteArray) {
        val b = binding.get() ?: return
        val linkHex = reachHex(linkId)
        lock.withLock {
            if (binding.get() !== b) return@withLock
            doc = doc.copy(links = doc.links.filterNot { it.linkIdHex == linkHex })
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
        }
    }

    /** The conversation groups this account is in: each carries a `member`
     *  key, and the setting decides its `group` key. */
    public fun setJoinedGroups(groups: Set<String>) {
        val b = binding.get() ?: return
        val normalised = groups.map { it.lowercase() }.sorted()
        if (normalised == doc.joinedGroups) return
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(joinedGroups = normalised)
                store.save(b.accountIdB64, doc)
                syncLocked(b, System.currentTimeMillis())
            }
        }
    }

    /** The channels this account is in: the setting decides their keys. */
    public fun setChannels(channels: Set<String>) {
        val b = binding.get() ?: return
        val normalised = channels.map { it.lowercase() }.sorted()
        if (normalised == doc.channels) return
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(channels = normalised)
                store.save(b.accountIdB64, doc)
                syncLocked(b, System.currentTimeMillis())
            }
        }
    }

    /**
     * Rotate a slot (ADR-0060 §1): bump its version, so the old key comes off
     * the mailbox and a new one goes on. `contact` and `fof` when a contact is
     * removed; `group` and `member` of a group that shrank (strictly after the
     * remove commit, C-61 shape); `channel` on a channel epoch.
     */
    public suspend fun rotate(kind: ReachKind, scopeHex: String = "") {
        val b = binding.get() ?: return
        val slot = ReachSlot(kind, if (kind.scopeless) "" else scopeHex.lowercase())
        val envelope = lock.withLock {
            if (binding.get() !== b) return
            val next = versionOf(doc, slot) + 1
            doc = doc.copy(
                versions = doc.versions.filterNot { it.kind == kind.name && it.scopeHex == slot.scopeHex } +
                    StoredVersion(kind.name, slot.scopeHex, next),
            )
            val envelope = stampReachStateLocked()
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
            envelope
        }
        // An envelope composed for an account that is gone is not sent
        // through the next account's devices.
        if (binding.get() !== b) return
        envelope?.let(::publishToDevices)
    }

    /** Compose the counters now held as a ReachState stamped strictly after
     *  the last one, and keep it as the state in force. Under [lock]. */
    private fun stampReachStateLocked(): ByteArray? {
        val previous = storedReachStateStamp()
        val state = FfiReachState(
            entries = reachStateEntries(versionsOf(doc)),
            updatedAtMs = nextSyncStamp(System.currentTimeMillis(), previous),
        )
        val envelope = try {
            uniffi.lakko_ffi_uniffi.composeReachStateEnvelope(state)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "reach state not composed: ${cause.javaClass.simpleName}")
            return null
        }
        doc = doc.copy(reachStateHex = reachHex(envelope))
        return envelope
    }

    private fun storedReachStateStamp(): ULong? = doc.reachStateHex?.let(::reachUnhex)?.let { wire ->
        runCatching { uniffi.lakko_ffi_uniffi.decodeReachStateEnvelope(wire).updatedAtMs }.getOrNull()
    }

    private fun versionsOf(doc: ReachDoc): Map<ReachSlot, Int> = doc.versions.mapNotNull { v ->
        val kind = runCatching { ReachKind.valueOf(v.kind) }.getOrNull() ?: return@mapNotNull null
        ReachSlot(kind, v.scopeHex) to v.version
    }.toMap()

    private fun publishToDevices(envelope: ByteArray) {
        val publisher = deviceSyncPublisher
        if (publisher == null) {
            Log.i(LOG_TAG, "device sync not running: change kept on this device")
            return
        }
        try {
            publisher(envelope)
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "device sync send failed: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * A device was just admitted to the DeviceGroup: send it the envelopes in
     * force here through [deviceSyncPublisher], the sync runtime's own
     * DeviceGroup handle. Never through a second handle of the same
     * DeviceGroup: two handles sealing at one sender generation reuse a key
     * and nonce, and the receiver refuses the second as a reuse.
     */
    public fun resendToDevices() {
        deviceSyncEnvelopes().forEach(::publishToDevices)
    }

    /** The Visibility and ReachState envelopes in force here, verbatim with
     *  their original stamps, and the own profile envelope with its name's
     *  stored stamp (ADR-0048 section 4a): what this device sends a device it
     *  admits to the DeviceGroup (ADR-0060 §7). Empty for what was never set. */
    public fun deviceSyncEnvelopes(): List<ByteArray> {
        val b = binding.get() ?: return emptyList()
        return deviceSyncEnvelopesToSend(doc.visibilityHex, doc.reachStateHex, ownProfileEnvelope(b))
    }

    /**
     * ADR-0048 section 4a — the own name was saved or cleared on this device:
     * send the own profile envelope to the account's other devices, with the
     * name's stored stamp. Nothing is sent when no name was ever saved, or
     * before a session binds (the hand-over to a device admitted later
     * carries it through [deviceSyncEnvelopes]).
     */
    public fun publishOwnProfileToDevices() {
        val b = binding.get() ?: return
        ownProfileEnvelope(b)?.let(::publishToDevices)
    }

    private fun ownProfileEnvelope(b: Binding): ByteArray? {
        val profiles = ownProfiles ?: return null
        val accountId = rawAccountId(b) ?: return null
        return me.lakko.app.profile.ownProfileDeviceEnvelope(profiles.load(accountId), accountId) {
            uniffi.lakko_ffi_uniffi.composeProfileEnvelope(it)
        }
    }

    private fun rawAccountId(b: Binding): ByteArray? =
        runCatching { me.lakko.app.auth.base64Decode(b.accountIdB64) }.getOrNull()

    /**
     * A DeviceGroup service envelope from another of the account's devices
     * (ADR-0060 §7). Kept only when it is a Visibility or a ReachState
     * STRICTLY newer than the one held; anything else is dropped. On
     * acceptance the mailbox is reconciled and [onVisibilityChanged] re-runs
     * what a local change would have (profiles, Everyone, introductions).
     * Before a session binds, it waits for [bind].
     */
    public fun adoptDeviceSync(envelope: ByteArray) {
        val b = binding.get()
        if (b == null) {
            pendingDeviceSync.add(envelope.copyOf())
            return
        }
        when (val received = classifyDeviceSync(envelope) { uniffi.lakko_ffi_uniffi.dispatchIncomingEnvelope(it) }) {
            is ReceivedDeviceSync.Visibility -> scope.launch { adoptVisibility(b, received) }
            is ReceivedDeviceSync.ReachState -> scope.launch { adoptReachState(b, received) }
            is ReceivedDeviceSync.Profile -> scope.launch { adoptOwnProfile(b, received) }
            ReceivedDeviceSync.Foreign -> Log.i(LOG_TAG, "device sync: foreign envelope dropped")
        }
    }

    private suspend fun adoptVisibility(b: Binding, received: ReceivedDeviceSync.Visibility) {
        val accepted = lock.withLock {
            if (binding.get() !== b) return
            val held = currentVisibility()?.updatedAtMs
            if (!keepsIncoming(received.setting.updatedAtMs, held) { i, s ->
                    uniffi.lakko_ffi_uniffi.visibilityIsNewer(i, s)
                }
            ) {
                return
            }
            // Kept verbatim: a later re-send carries the original stamp.
            doc = doc.copy(visibilityHex = reachHex(received.wire))
            mutableVisibility.value = received.setting
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
            received.setting
        }
        rerunAfterAdopt(accepted)
    }

    /** ADR-0048 section 4a — keep the own name from a sibling when its stamp
     *  is strictly newer; one naming another account is dropped. Not
     *  republished to groups: the device that made the change already has. */
    private suspend fun adoptOwnProfile(b: Binding, received: ReceivedDeviceSync.Profile) {
        val profiles = ownProfiles ?: return
        lock.withLock {
            if (binding.get() !== b) return
            val accountId = rawAccountId(b) ?: return
            val adopted = profiles.update(accountId) { stored ->
                me.lakko.app.profile.adoptOwnProfileFromDevice(stored, received.profile, accountId)
            }
            if (!adopted) {
                Log.i(LOG_TAG, "device sync: own profile not newer or not this account's")
                return
            }
        }
        onOwnNameAdopted?.invoke()
    }

    private suspend fun adoptReachState(b: Binding, received: ReceivedDeviceSync.ReachState) {
        val adoption = lock.withLock {
            if (binding.get() !== b) return
            val decided = adoptReachStateMerge(
                stored = storedReachState(),
                incoming = received.state,
                nowMs = System.currentTimeMillis(),
                merge = { stored, incoming, now -> uniffi.lakko_ffi_uniffi.mergeReachState(stored, incoming, now) },
                compose = { uniffi.lakko_ffi_uniffi.composeReachStateEnvelope(it) },
            )
            val versions = decided.versions
            val wire = decided.storeWire
            if (versions != null && wire != null) {
                // Persisted exactly as the core returned it, stamp included.
                doc = doc.copy(
                    versions = versions.map { (slot, v) -> StoredVersion(slot.kind.name, slot.scopeHex, v) },
                    reachStateHex = reachHex(wire),
                )
                store.save(b.accountIdB64, doc)
                syncLocked(b, System.currentTimeMillis())
            }
            decided
        }
        if (binding.get() !== b) return
        // Sent as returned, never re-stamped here.
        adoption.sendWire?.let(::publishToDevices)
        if (adoption.storeWire != null) currentVisibility()?.let { rerunAfterAdopt(it) }
    }

    /** The ReachState in force here: the stored envelope, or (never set) the
     *  counters held at stamp 0. */
    private fun storedReachState(): FfiReachState =
        doc.reachStateHex?.let(::reachUnhex)?.let { wire ->
            runCatching { uniffi.lakko_ffi_uniffi.decodeReachStateEnvelope(wire) }.getOrNull()
        } ?: FfiReachState(entries = reachStateEntries(versionsOf(doc)), updatedAtMs = 0uL)

    private suspend fun rerunAfterAdopt(setting: FfiVisibilitySetting) {
        try {
            onVisibilityChanged?.invoke(setting)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "applying a synced change failed: ${cause.javaClass.simpleName}")
        }
    }

    /** Reconcile the mailbox with the desired key set, in the background. */
    public fun launchSync() {
        val b = binding.get() ?: return
        scope.launch { lock.withLock { syncLocked(b, System.currentTimeMillis()) } }
    }

    /** Reconcile now. `true` when the mailbox holds the desired set. */
    public suspend fun sync(): Boolean {
        val b = binding.get() ?: return false
        return lock.withLock { syncLocked(b, System.currentTimeMillis()) }
    }

    /** The version counters, as the ReachState envelope carries them, with
     *  the stamp of the state in force (0 before any rotation). */
    public fun reachState(): FfiReachState = FfiReachState(
        entries = reachStateEntries(versionsOf(doc)),
        updatedAtMs = storedReachStateStamp() ?: 0uL,
    )

    private suspend fun syncLocked(b: Binding, nowMs: Long): Boolean {
        if (binding.get() !== b) return false
        val recovery = ensureVersionRecoveryLocked(b)
        if (binding.get() !== b) return false
        val setting = mutableVisibility.value ?: uniffi.lakko_ffi_uniffi.defaultVisibilitySetting()
        val inputs = inputsFor(doc, setting)
        val wanted = desiredReachKeys(inputs, nowMs).let {
            if (recovery == null) it else registrableBeforeRecovery(it)
        }
        val desired = wanted.associateBy { key ->
            reachHex(publicKeyOf(derive(b, key.slot, key.version).seed))
        }
        val registered = doc.registered.mapNotNull { r ->
            val kind = runCatching { ReachKind.valueOf(r.kind) }.getOrNull() ?: return@mapNotNull null
            r.pkHex to RegisteredReachKey(ReachSlot(kind, r.scopeHex), r.version, r.expiresAtMs)
        }.toMap()
        val diff = diffReachKeys(desired, registered)
        val wantOpen = setting.mode == FfiVisibilityMode.EVERYONE
        // The Everyone flag waits for the recovery too: it is part of the
        // setting the versions are reconciled with.
        val openChange = if (recovery != null || doc.registeredOpen == wantOpen) null else wantOpen
        // Expired replies drop out of the document once the diff removed them.
        val liveReplies = doc.replies.filter { it.expiresAtMs > nowMs }
        if (diff.isEmpty && openChange == null) {
            if (liveReplies.size != doc.replies.size) {
                doc = doc.copy(replies = liveReplies)
                store.save(b.accountIdB64, doc)
            }
            mutableStatus.value = recovery ?: ReachApplyStatus.APPLIED
            // While the recovery is pending this reports the restricted set
            // (`link` / `reply` keys only), so a link or reply mint proceeds;
            // the status says the rest is not applied.
            return true
        }
        mutableStatus.value = ReachApplyStatus.APPLYING
        return try {
            dsFactory().use { ds ->
                ds.putMailboxReach(
                    pseudonym = b.mailbox,
                    signer = b.mailboxSigner,
                    add = diff.add.map { (pk, key) -> DsReachKeyAdd(reachUnhex(pk)!!, key.expiresAtMs) },
                    remove = diff.remove.mapNotNull(::reachUnhex),
                    open = openChange,
                )
            }
            // The account signed out (or another bound) while the request was
            // out: [doc] is no longer this account's document. Writing it
            // here would save an empty or a foreign document under this
            // account, losing its setting and its version counters.
            if (binding.get() !== b) return false
            doc = doc.copy(
                registered = desired.map { (pk, key) ->
                    StoredRegistered(pk, key.slot.kind.name, key.slot.scopeHex, key.version, key.expiresAtMs)
                },
                registeredOpen = if (openChange == null) doc.registeredOpen else wantOpen,
                replies = liveReplies,
            )
            store.save(b.accountIdB64, doc)
            mutableStatus.value = recovery ?: ReachApplyStatus.APPLIED
            // Pending: only the restricted set went out (see above).
            true
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: DsNetworkError) {
            Log.i(LOG_TAG, "reach keys not registered yet: offline")
            mutableStatus.value = ReachApplyStatus.OFFLINE
            false
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "reach keys not registered: ${cause.javaClass.simpleName}")
            mutableStatus.value = ReachApplyStatus.FAILED
            false
        }
    }

    /**
     * ADR-0060 §1 ("Versions after a restore", amended 2026-10-01). Under [lock].
     *
     * A device that holds no version state ([holdsNoVersionState]: a fresh
     * install, or a restore with no other device alive) must not start every
     * class at version 0, or a key rotated away (a removed contact, an evicted
     * member) would be registered again. So before it registers anything
     * versioned it asks the DS which keys the mailbox holds (owner-only, the
     * mailbox proof) and each slot resumes at the highest version whose key is
     * listed ([recoverVersions]); a slot with none starts at 0.
     *
     * Returns `null` when versioned keys may be registered, or the status to
     * show while the read has not succeeded. Fails CLOSED meanwhile: no
     * versioned key and no Everyone flag go out (only `link` / `reply` keys,
     * which carry no history); the keys the mailbox already holds stay as they
     * are on the server, so whoever holds a current grant can still write. The
     * read is retried after [VERSION_RECOVERY_RETRY_MS] and on every reconcile.
     *
     * Slots this device learns later (a group it is re-admitted to after the
     * restore) are recovered from the same server snapshot the first time they
     * appear without a version of their own.
     */
    private suspend fun ensureVersionRecoveryLocked(b: Binding): ReachApplyStatus? {
        var state = doc.versionRecovery?.let { runCatching { VersionRecovery.valueOf(it) }.getOrNull() }
        if (state == null) {
            state = if (holdsNoVersionState(
                    versionsEmpty = doc.versions.isEmpty(),
                    registeredEmpty = doc.registered.isEmpty(),
                    reachStateKnown = doc.reachStateHex != null,
                )
            ) {
                VersionRecovery.PENDING
            } else {
                VersionRecovery.DONE
            }
            doc = doc.copy(versionRecovery = state.name)
            store.save(b.accountIdB64, doc)
        }
        if (state == VersionRecovery.PENDING) {
            val listed = try {
                dsFactory().use { ds -> ds.fetchMailboxReach(b.mailbox, b.mailboxSigner) }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.i(LOG_TAG, "reach versions not recovered yet (${cause.javaClass.simpleName}); retried later")
                scheduleRecoveryRetry(b)
                return if (cause is DsNetworkError) ReachApplyStatus.OFFLINE else ReachApplyStatus.FAILED
            }
            if (binding.get() !== b) return ReachApplyStatus.APPLYING
            doc = doc.copy(
                versionRecovery = VersionRecovery.DONE.name,
                recoveryPkHex = listed.keys.map { reachHex(it.publicKey) }.distinct(),
            )
            store.save(b.accountIdB64, doc)
            Log.i(LOG_TAG, "reach versions recovered from ${listed.keys.size} registered keys")
        }
        applyRecoveredSlotsLocked(b)
        return null
    }

    /** Recover the version of every nameable slot that has none yet and was
     *  not scanned before, from [ReachDoc.recoveryPkHex]. Under [lock]. */
    private fun applyRecoveredSlotsLocked(b: Binding) {
        if (doc.recoveryPkHex.isEmpty()) return
        val scanned = doc.recoveredSlots.toHashSet()
        val versioned = doc.versions.map { it.kind + ":" + it.scopeHex }.toHashSet()
        val todo = recoverableSlots(doc.joinedGroups, doc.channels).filter {
            val id = reachSlotId(it)
            id !in scanned && id !in versioned
        }
        if (todo.isEmpty()) return
        val found = recoverVersions(
            slots = todo,
            registered = doc.recoveryPkHex.toHashSet(),
            publicKeyHexAt = { slot, version ->
                runCatching { reachHex(publicKeyOf(derive(b, slot, version).seed)) }.getOrNull()
            },
        )
        doc = doc.copy(
            versions = doc.versions + found.filterValues { it > 0 }.map { (slot, v) ->
                StoredVersion(slot.kind.name, slot.scopeHex, v)
            },
            recoveredSlots = doc.recoveredSlots + todo.map(::reachSlotId),
        )
        store.save(b.accountIdB64, doc)
    }

    private val recoveryRetryScheduled = java.util.concurrent.atomic.AtomicBoolean(false)

    private fun scheduleRecoveryRetry(b: Binding) {
        if (!recoveryRetryScheduled.compareAndSet(false, true)) return
        scope.launch {
            try {
                kotlinx.coroutines.delay(VERSION_RECOVERY_RETRY_MS)
            } finally {
                recoveryRetryScheduled.set(false)
            }
            if (binding.get() === b) lock.withLock { syncLocked(b, System.currentTimeMillis()) }
        }
    }

    private fun inputsFor(doc: ReachDoc, setting: FfiVisibilitySetting): ReachInputs {
        val versions = doc.versions.mapNotNull { v ->
            val kind = runCatching { ReachKind.valueOf(v.kind) }.getOrNull() ?: return@mapNotNull null
            ReachSlot(kind, v.scopeHex) to v.version
        }.toMap()
        val joined = doc.joinedGroups.toSet()
        val visibleGroups = joined.filter { hex ->
            val id = reachUnhex(hex) ?: return@filter false
            allows(setting, FfiScopeKind.GROUP, id)
        }.toSet()
        val visibleChannels = doc.channels.filter { hex ->
            val id = reachUnhex(hex) ?: return@filter false
            allows(setting, FfiScopeKind.CHANNEL, id)
        }.toSet()
        return ReachInputs(
            versions = versions,
            joinedGroups = joined,
            visibleGroups = visibleGroups,
            visibleChannels = visibleChannels,
            contactsOfContacts = runCatching { uniffi.lakko_ffi_uniffi.allowsContactsOfContacts(setting) }
                .getOrDefault(false),
            links = doc.links.associate { it.linkIdHex to it.expiresAtMs },
            replies = doc.replies.associate { it.scopeHex to it.expiresAtMs },
        )
    }

    // -----------------------------------------------------------------
    // The setting
    // -----------------------------------------------------------------

    /** The setting now in force here (the default for an account that never
     *  chose), or `null` before a session binds. */
    public fun currentVisibility(): FfiVisibilitySetting? = mutableVisibility.value
        ?: binding.get()?.let { uniffi.lakko_ffi_uniffi.defaultVisibilitySetting() }

    /**
     * Apply a setting the person chose (UX section 3: takes effect at once, no
     * Save button). Stamped strictly after the one it replaces, persisted, the
     * mailbox reconciled, then [onVisibilityChanged] runs.
     */
    public suspend fun updateVisibility(chosen: FfiVisibilitySetting) {
        val b = binding.get() ?: return
        val previous = currentVisibility()
        val stamp = maxOf(System.currentTimeMillis().toULong(), (previous?.updatedAtMs ?: 0uL) + 1uL)
        val stamped = chosen.copy(updatedAtMs = stamp)
        val envelope = uniffi.lakko_ffi_uniffi.composeVisibilityEnvelope(stamped)
        // The chosen row shows at once (UX section 9, "Applying"); the server
        // catches up under the lock.
        mutableVisibility.value = stamped
        mutableStatus.value = ReachApplyStatus.APPLYING
        lock.withLock {
            if (binding.get() !== b) return
            doc = doc.copy(visibilityHex = reachHex(envelope))
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
        }
        // Signed out while the mailbox was reconciled: the setting is saved
        // for its account; nothing is sent as whoever is bound now.
        if (binding.get() !== b) return
        // ADR-0060 §7 — the account's other devices take the same setting.
        publishToDevices(envelope)
        try {
            onVisibilityChanged?.invoke(stamped)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "applying the setting to profiles failed: ${cause.javaClass.simpleName}")
        }
    }

    /** Retry after a failure (the screen's Retry action). */
    public suspend fun retryApply() {
        sync()
        mutableVisibility.value?.let { setting ->
            try {
                onVisibilityChanged?.invoke(setting)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                // Reported through the status.
            }
        }
    }

    /** Whether the setting lets the members of group [groupHex] find this
     *  person (their `group` grant and username go in the profile there). */
    public fun groupVisible(groupHex: String): Boolean {
        val setting = currentVisibility() ?: return false
        val id = reachUnhex(groupHex) ?: return false
        return allows(setting, FfiScopeKind.GROUP, id)
    }

    /** Every group and channel this account is in, as setting scopes (what
     *  "turning a group off under All" keeps on). */
    public fun knownScopes(): List<FfiScopeId> =
        doc.joinedGroups.mapNotNull { hex -> reachUnhex(hex)?.let { FfiScopeId(FfiScopeKind.GROUP, it) } } +
            doc.channels.mapNotNull { hex -> reachUnhex(hex)?.let { FfiScopeId(FfiScopeKind.CHANNEL, it) } }

    /** Whether the setting lets the members of channel [channelHex] find this person. */
    public fun channelVisible(channelHex: String): Boolean {
        val setting = currentVisibility() ?: return false
        val id = reachUnhex(channelHex) ?: return false
        return allows(setting, FfiScopeKind.CHANNEL, id)
    }

    /**
     * The grants and whether to state the username in a profile published in
     * [groupHex] (ADR-0060 §3): a one-to-one carries `contact` + `member` and
     * the username; a group carries `member` always, and `group` + the
     * username only where the setting lets its members find this person.
     */
    public fun profileGrants(groupHex: String, oneToOne: Boolean): ProfileGrants {
        val scope = reachUnhex(groupHex) ?: return ProfileGrants(emptyList(), shareUsername = oneToOne)
        val member = grant(ReachKind.MEMBER, scope)
        if (oneToOne) {
            return ProfileGrants(listOfNotNull(grant(ReachKind.CONTACT), member), shareUsername = true)
        }
        val visible = groupVisible(groupHex)
        return ProfileGrants(
            grants = listOfNotNull(if (visible) grant(ReachKind.GROUP, scope) else null, member),
            shareUsername = visible,
        )
    }

    /** The username certificate discovery signed for this account (ADR-0060
     *  §5), or `null` until one was fetched. */
    public fun usernameCertificate(): ByteArray? = doc.usernameCertHex?.let(::reachUnhex)

    /** Keep the certificate from `POST /discovery/register` or
     *  `GET /discovery/username-certificate`. */
    public fun keepUsernameCertificate(certificate: ByteArray): Boolean {
        val b = binding.get() ?: return false
        val hex = reachHex(certificate)
        if (doc.usernameCertHex == hex) return false
        doc = doc.copy(usernameCertHex = hex)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(usernameCertHex = hex)
                store.save(b.accountIdB64, doc)
            }
        }
        return true
    }

    /** Whether the Everyone publication was last sent as public, and for
     *  which [everyonePublishKey] (ADR-0060 §5). */
    public fun publishedEveryone(): Pair<Boolean?, String?> = doc.publishedPublic to doc.publishedKey

    /** Record what the Everyone publication now holds ([key] is `null` when
     *  not public). */
    public fun notePublishedEveryone(public: Boolean, key: String?) {
        val b = binding.get() ?: return
        doc = doc.copy(publishedPublic = public, publishedKey = key)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(publishedPublic = public, publishedKey = key)
                store.save(b.accountIdB64, doc)
            }
        }
    }

    // -----------------------------------------------------------------
    // Grants other people handed this device
    // -----------------------------------------------------------------

    /** Keep a grant [ownerKey] handed out, received at [scopeHex] (the group
     *  or channel id for those classes). The caller already ran
     *  [acceptReceivedGrant]. A newer version replaces an older one. */
    public fun keepHeld(ownerKey: ByteArray, grant: FfiReachGrant, scopeHex: String) {
        val b = binding.get() ?: return
        scope.launch { storeHeld(b, ownerKey, grant, scopeHex) }
    }

    /** [keepHeld], awaited: the grant is in the store when this returns
     *  (the member-card reader checks what it holds right after filing). */
    public suspend fun fileHeld(ownerKey: ByteArray, grant: FfiReachGrant, scopeHex: String) {
        val b = binding.get() ?: return
        storeHeld(b, ownerKey, grant, scopeHex)
    }

    private suspend fun storeHeld(b: Binding, ownerKey: ByteArray, grant: FfiReachGrant, scopeHex: String) {
        val kind = grant.`class`.toKind()
        val ownerHex = reachHex(ownerKey)
        val scopeKey = if (kind == ReachKind.GROUP || kind == ReachKind.MEMBER || kind == ReachKind.CHANNEL) {
            scopeHex.lowercase()
        } else {
            ""
        }
        val wire = try {
            uniffi.lakko_ffi_uniffi.encodeReachGrant(grant)
        } catch (_: Throwable) {
            return
        }
        val changed = lock.withLock {
            if (binding.get() !== b) return@withLock false
            val existing = doc.held.firstOrNull {
                it.ownerKeyHex == ownerHex && it.kind == kind.name && it.scopeHex == scopeKey
            }
            if (existing != null && existing.grantHex == reachHex(wire)) return@withLock false
            if (existing != null) {
                val old = runCatching {
                    uniffi.lakko_ffi_uniffi.decodeReachGrant(reachUnhex(existing.grantHex)!!)
                }.getOrNull()
                if (old != null && old.version > grant.version) return@withLock false
            }
            doc = doc.copy(
                held = doc.held.filterNot {
                    it.ownerKeyHex == ownerHex && it.kind == kind.name && it.scopeHex == scopeKey
                } + StoredHeld(ownerHex, kind.name, scopeKey, reachHex(wire)),
            )
            store.save(b.accountIdB64, doc)
            true
        }
        if (changed) notifyGrantHeld(ownerKey)
    }

    /** Drop the grant of [kind] held for [ownerKey] at [scopeHex] (a member
     *  card that no longer carries its `channel` grant, ADR-0060 §3). */
    public suspend fun withdrawHeld(ownerKey: ByteArray, kind: ReachKind, scopeHex: String) {
        val b = binding.get() ?: return
        val ownerHex = reachHex(ownerKey)
        val scopeKey = scopeHex.lowercase()
        lock.withLock {
            if (binding.get() !== b) return@withLock
            val kept = doc.held.filterNot {
                it.ownerKeyHex == ownerHex && it.kind == kind.name && it.scopeHex == scopeKey
            }
            if (kept.size == doc.held.size) return@withLock
            doc = doc.copy(held = kept)
            store.save(b.accountIdB64, doc)
        }
    }

    /** Whether a grant of [kind] is held for [ownerKey] at [scopeHex]. */
    public fun holdsHeld(ownerKey: ByteArray, kind: ReachKind, scopeHex: String): Boolean {
        val ownerHex = reachHex(ownerKey)
        val scopeKey = scopeHex.lowercase()
        return doc.held.any { it.ownerKeyHex == ownerHex && it.kind == kind.name && it.scopeHex == scopeKey }
    }

    /**
     * The widest grant held for [ownerKey] as its wire, or `null` (web
     * `bestGrantFor`): `contact`, else a `group`, else a `channel`. A bare
     * `member` key is never one: a co-member the person hides from holds it
     * (UX section 6.2). SECRET.
     */
    public fun bestHeldGrantWire(ownerKey: ByteArray): ByteArray? {
        val held = heldFor(ownerKey)
        for (kind in listOf(ReachKind.CONTACT, ReachKind.GROUP, ReachKind.CHANNEL)) {
            val pick = held.filter { it.first.kind == kind }.minByOrNull { it.first.scopeHex } ?: continue
            return runCatching { uniffi.lakko_ffi_uniffi.encodeReachGrant(pick.second) }.getOrNull()
        }
        return null
    }

    /** Drop every grant held for [ownerKey] (a contact removed). */
    public fun forgetHeld(ownerKey: ByteArray) {
        val b = binding.get() ?: return
        val ownerHex = reachHex(ownerKey)
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(held = doc.held.filterNot { it.ownerKeyHex == ownerHex })
                store.save(b.accountIdB64, doc)
            }
        }
    }

    /** The grants held for [ownerKey], newest wire only. SECRET. */
    public fun heldFor(ownerKey: ByteArray): List<Pair<HeldGrantRef, FfiReachGrant>> {
        val ownerHex = reachHex(ownerKey)
        return doc.held.filter { it.ownerKeyHex == ownerHex }.mapNotNull { h ->
            val kind = runCatching { ReachKind.valueOf(h.kind) }.getOrNull() ?: return@mapNotNull null
            val grant = runCatching { uniffi.lakko_ffi_uniffi.decodeReachGrant(reachUnhex(h.grantHex)!!) }
                .getOrNull() ?: return@mapNotNull null
            HeldGrantRef(kind, h.scopeHex) to grant
        }
    }

    /**
     * What signs a write of [purpose] into [ownerKey]'s mailbox, or `null`
     * (unsigned — which an open mailbox takes and a closed one drops).
     *
     * This account's own mailbox is written with its own `contact` key (the
     * account's other devices, ADR-0050). A Welcome into [groupHex] prefers a
     * committer delegation this device holds for that person there.
     */
    public fun reachAuthFor(
        ownerKey: ByteArray,
        purpose: WritePurpose,
        groupHex: String? = null,
    ): DsReachAuth? {
        for (candidate in reachAuthCandidates(ownerKey, purpose, groupHex)) {
            candidate()?.let { return it }
        }
        return null
    }

    /**
     * Every authorisation this device holds for a write of [purpose] into
     * [ownerKey]'s mailbox, in [reachAuthFor]'s order, each built only when
     * asked for (a signer is a native object). ADR-0050 section 2.5a item 7:
     * a Welcome writer tries them in turn and moves on after a `401`. This
     * account's own mailbox yields its own `contact` key alone. Empty before a
     * session binds or when nothing is held.
     */
    public fun reachAuthCandidates(
        ownerKey: ByteArray,
        purpose: WritePurpose,
        groupHex: String? = null,
    ): List<() -> DsReachAuth?> {
        val b = binding.get() ?: return emptyList()
        if (ownerKey.contentEquals(b.accountKey)) {
            return listOf({ grant(ReachKind.CONTACT)?.let { DsReachAuth(reachSigner(it.seed)) } })
        }
        val held = heldFor(ownerKey)
        val out = ArrayList<() -> DsReachAuth?>()
        for (ref in orderedWriteGrants(held.map { it.first }, purpose, groupHex?.lowercase())) {
            val grant = held.first { it.first == ref }.second
            out += { DsReachAuth(reachSigner(grant.seed)) }
        }
        if (purpose == WritePurpose.WELCOME && groupHex != null) {
            out += {
                delegationFor(groupHex, ownerKey)?.let { header ->
                    DsReachAuth(reachSigner(b.accountSeed.copyOf()), delegationHeader = header)
                }
            }
        }
        return out
    }

    /** ADR-0050 section 2.5a item 7 — told the owner key of a grant (or a
     *  committer delegation naming its account) this device just kept, so a
     *  Welcome owed to that person is retried at once. Installed by the chat
     *  repository; must not block. */
    @Volatile
    public var onGrantHeld: ((ownerKey: ByteArray) -> Unit)? = null

    private fun notifyGrantHeld(ownerKey: ByteArray) {
        try {
            onGrantHeld?.invoke(ownerKey.copyOf())
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "grant-held hook failed: ${cause.javaClass.simpleName}")
        }
    }

    // -----------------------------------------------------------------
    // Contacts of contacts (ADR-0060 §4)
    // -----------------------------------------------------------------

    /** Keep the introduction record [senderKey] offered in the one-to-one
     *  conversation (the caller checked it came from the other participant).
     *  The newest per contact wins. */
    public suspend fun keepIntroductionOffer(
        senderKey: ByteArray,
        record: ByteArray,
        /** The account the offer was received for; `null` = whoever is bound. */
        boundAs: BoundAccount? = null,
    ) {
        val b = binding.get() ?: return
        if (boundAs != null && boundAs.token !== b) return
        val senderHex = reachHex(senderKey)
        val recordHex = reachHex(record)
        lock.withLock {
            if (binding.get() !== b || doc.introOffers[senderHex] == recordHex) return@withLock
            doc = doc.copy(introOffers = doc.introOffers + (senderHex to recordHex))
            store.save(b.accountIdB64, doc)
        }
    }

    /** Keep the bundle [senderKey] relayed, when strictly newer than the one
     *  held (`bundleIsNewer`: a re-send carries its original stamp). */
    public suspend fun keepIntroductionsBundle(
        senderKey: ByteArray,
        wire: ByteArray,
        updatedAtMs: ULong,
        /** The account the bundle was received for; `null` = whoever is bound. */
        boundAs: BoundAccount? = null,
    ) {
        val b = binding.get() ?: return
        if (boundAs != null && boundAs.token !== b) return
        val senderHex = reachHex(senderKey)
        lock.withLock {
            if (binding.get() !== b) return@withLock
            val held = doc.introBundles[senderHex]
            if (held != null &&
                !uniffi.lakko_ffi_uniffi.bundleIsNewer(updatedAtMs, held.updatedAtMs.toULong())
            ) {
                return@withLock
            }
            doc = doc.copy(
                introBundles = doc.introBundles + (senderHex to StoredIntroBundle(reachHex(wire), updatedAtMs.toLong())),
            )
            store.save(b.accountIdB64, doc)
        }
    }

    /** Every relayed bundle wire this device keeps. */
    public fun introductionBundleWires(): List<ByteArray> =
        doc.introBundles.toSortedMap().values.mapNotNull { reachUnhex(it.bundleHex) }

    /** Every record the contacts offered, hex, ordered by contact key. */
    public fun offeredIntroductionRecordsHex(): List<String> = doc.introOffers.toSortedMap().values.toList()

    /** The contacts-of-contacts publication state held here. */
    internal fun introPublicationState(): IntroPublicationState = IntroPublicationState(
        termOutputs = doc.termOutputs,
        ownOffer = doc.ownOffer,
        offerSentTo = doc.offerSentTo,
        bundleSentTo = doc.bundleSentTo,
    )

    /** Persist the publication state after a step. */
    internal suspend fun saveIntroPublicationState(state: IntroPublicationState) {
        val b = binding.get() ?: return
        lock.withLock {
            if (binding.get() !== b) return@withLock
            doc = doc.copy(
                termOutputs = state.termOutputs,
                ownOffer = state.ownOffer,
                offerSentTo = state.offerSentTo,
                bundleSentTo = state.bundleSentTo,
            )
            store.save(b.accountIdB64, doc)
        }
    }

    /** The current version of the `fof` slot. */
    public fun fofVersion(): Int = versionOf(doc, ReachSlot(ReachKind.FOF, ""))

    // -----------------------------------------------------------------
    // Channels (ADR-0060 §3)
    // -----------------------------------------------------------------

    /**
     * The `channel` key is versioned by the channel epoch: raise the slot of
     * [channelHex] to [epoch] when it is behind (never lowers it), so the
     * mailbox carries the key of the epoch the member card names (web
     * `channelEpoch`).
     */
    public suspend fun raiseChannelVersion(channelHex: String, epoch: Int) {
        val b = binding.get() ?: return
        val slot = ReachSlot(ReachKind.CHANNEL, channelHex.lowercase())
        val envelope = lock.withLock {
            if (binding.get() !== b) return
            if (versionOf(doc, slot) >= epoch) {
                syncLocked(b, System.currentTimeMillis())
                return
            }
            doc = doc.copy(
                versions = doc.versions.filterNot { it.kind == slot.kind.name && it.scopeHex == slot.scopeHex } +
                    StoredVersion(slot.kind.name, slot.scopeHex, epoch),
            )
            val envelope = stampReachStateLocked()
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
            envelope
        }
        // An envelope composed for an account that is gone is not sent
        // through the next account's devices.
        if (binding.get() !== b) return
        envelope?.let(::publishToDevices)
    }

    /**
     * Seal this account's member card (ADR-0060 §3): [profile] (a profile
     * envelope) under the epoch [seed], bound to the account identity, or
     * `null` without a session (a reader drops an unbound card).
     */
    public fun sealOwnMemberCard(
        seed: ByteArray,
        epoch: UInt,
        channelId: ByteArray,
        subscriberPk: ByteArray,
        profile: ByteArray,
    ): ByteArray? {
        val b = binding.get() ?: return null
        return uniffi.lakko_ffi_uniffi.sealMemberCard(seed, epoch, channelId, subscriberPk, b.accountSeed, profile)
    }

    // -----------------------------------------------------------------
    // Committer delegations (ADR-0060 §6)
    // -----------------------------------------------------------------

    /** Keep the newest delegation envelope per (sender, added account) in a
     *  group, the sender resolved at receipt (C-68). */
    public fun keepDelegation(groupHex: String, senderKey: ByteArray, envelope: ByteArray) {
        val b = binding.get() ?: return
        val decoded = try {
            uniffi.lakko_ffi_uniffi.decodeCommitterDelegationEnvelope(envelope)
        } catch (_: Throwable) {
            return
        }
        val entry = StoredDelegation(
            groupHex = groupHex.lowercase(),
            senderKeyHex = reachHex(senderKey),
            addedAccountKeyHex = reachHex(decoded.addedAccountKey),
            envelopeHex = reachHex(envelope),
        )
        scope.launch {
            lock.withLock {
                if (binding.get() !== b) return@withLock
                doc = doc.copy(
                    delegations = doc.delegations.filterNot {
                        it.groupHex == entry.groupHex && it.senderKeyHex == entry.senderKeyHex &&
                            it.addedAccountKeyHex == entry.addedAccountKeyHex
                    } + entry,
                )
                store.save(b.accountIdB64, doc)
            }
            notifyGrantHeld(decoded.addedAccountKey)
        }
    }

    /** The delegation header naming this account for a Welcome to
     *  [addedAccountKey] in [groupHex], or `null`. */
    public fun delegationFor(groupHex: String, addedAccountKey: ByteArray): String? {
        val b = binding.get() ?: return null
        val addedHex = reachHex(addedAccountKey)
        for (d in doc.delegations.filter { it.groupHex == groupHex.lowercase() && it.addedAccountKeyHex == addedHex }) {
            val env = runCatching {
                uniffi.lakko_ffi_uniffi.decodeCommitterDelegationEnvelope(reachUnhex(d.envelopeHex)!!)
            }.getOrNull() ?: continue
            val header = runCatching { uniffi.lakko_ffi_uniffi.committerDelegationFor(env, b.accountKey) }
                .getOrNull()
            if (header != null) return header
        }
        return null
    }

    /**
     * The committer-delegation envelope a contact of [addedAccountKey] sends
     * next to an Add it proposed (ADR-0060 §6): one delegation per committer
     * candidate ([candidateKeys], owner first then admins by rank, at most
     * 16), each to that candidate's account key, living [ttlMs] (at most 24 h).
     * `null` when this device holds no `contact` grant for the added person.
     */
    public fun composeDelegations(
        addedAccountKey: ByteArray,
        candidateKeys: List<ByteArray>,
        ttlMs: Long = DELEGATION_TTL_MS,
        nowMs: Long = System.currentTimeMillis(),
    ): ByteArray? {
        val contact = heldFor(addedAccountKey).firstOrNull { it.first.kind == ReachKind.CONTACT }?.second
            ?: return null
        val mailbox = uniffi.lakko_ffi_uniffi.IdentityPseudonym.fromIdentity(addedAccountKey).use { it.asBytes() }
        val expires = (nowMs + ttlMs).toULong()
        val max = uniffi.lakko_ffi_uniffi.maxCommitterDelegations().toInt()
        val blobs = candidateKeys.take(max).map { candidate ->
            val nonce = ByteArray(DELEGATION_NONCE_LEN).also(random::nextBytes)
            uniffi.lakko_ffi_uniffi.signDelegationBlob(contact.seed, mailbox, candidate, expires, nonce)
        }
        if (blobs.isEmpty()) return null
        return uniffi.lakko_ffi_uniffi.composeCommitterDelegationEnvelope(
            uniffi.lakko_ffi_uniffi.FfiCommitterDelegationEnvelope(
                addedAccountKey = addedAccountKey,
                delegations = blobs,
            ),
        )
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    private fun derive(b: Binding, slot: ReachSlot, version: Int): FfiReachGrant =
        uniffi.lakko_ffi_uniffi.deriveReachGrant(
            b.accountSeed,
            slot.kind.toFfi(),
            if (slot.kind.scopeless) ByteArray(0) else reachUnhex(slot.scopeHex) ?: ByteArray(0),
            version.toUInt(),
        )

    private fun publicKeyOf(seed: ByteArray): ByteArray = uniffi.lakko_ffi_uniffi.reachVerifyingKey(seed)

    private fun versionOf(doc: ReachDoc, slot: ReachSlot): Int =
        doc.versions.firstOrNull { it.kind == slot.kind.name && it.scopeHex == slot.scopeHex }?.version ?: 0

    private fun allows(setting: FfiVisibilitySetting, kind: FfiScopeKind, id: ByteArray): Boolean =
        runCatching { uniffi.lakko_ffi_uniffi.allowsScope(setting, FfiScopeId(kind, id)) }.getOrDefault(false)

    private fun decodeVisibility(hex: String?): FfiVisibilitySetting? {
        val bytes = hex?.let(::reachUnhex) ?: return uniffi.lakko_ffi_uniffi.defaultVisibilitySetting()
        return runCatching { uniffi.lakko_ffi_uniffi.decodeVisibilityEnvelope(bytes) }
            .getOrElse { uniffi.lakko_ffi_uniffi.defaultVisibilitySetting() }
    }

    public companion object {
        private const val LOG_TAG = "Reach"

        /** How long after a failed version-recovery read it is tried again
         *  (besides every reconcile). Chosen, not measured. */
        public const val VERSION_RECOVERY_RETRY_MS: Long = 60_000L

        /** How long a committer delegation lives: the 24-hour maximum. */
        public const val DELEGATION_TTL_MS: Long = 24L * 60 * 60 * 1000

        private const val DELEGATION_NONCE_LEN = 16

        @Volatile
        private var instance: ReachManager? = null

        /** The process-wide manager. */
        public fun get(context: Context): ReachManager =
            instance ?: synchronized(this) {
                instance ?: ReachManager(
                    store = EncryptedReachDocStore(context),
                    ownProfiles = ContextOwnProfileAccess(context.applicationContext),
                ).also { instance = it }
            }

        /** The manager if one was created, else `null` (background readers). */
        public fun peek(): ReachManager? = instance
    }
}

/** [ReachManager.OwnProfileAccess] over [me.lakko.app.profile.OwnProfileStore]. */
private class ContextOwnProfileAccess(private val context: Context) : ReachManager.OwnProfileAccess {
    override fun load(accountId: ByteArray): me.lakko.app.profile.OwnProfile? =
        me.lakko.app.profile.OwnProfileStore.load(context, accountId)

    override fun update(
        accountId: ByteArray,
        transform: (me.lakko.app.profile.OwnProfile?) -> me.lakko.app.profile.OwnProfile?,
    ): Boolean = me.lakko.app.profile.OwnProfileStore.update(context, accountId, transform)
}

/** What a profile carries (ADR-0060 §3). SECRET grants. */
public data class ProfileGrants(val grants: List<FfiReachGrant>, val shareUsername: Boolean)

/** [ReachKind] as the FFI's class. */
public fun ReachKind.toFfi(): FfiReachClass = when (this) {
    ReachKind.CONTACT -> FfiReachClass.CONTACT
    ReachKind.LINK -> FfiReachClass.LINK
    ReachKind.REPLY -> FfiReachClass.REPLY
    ReachKind.GROUP -> FfiReachClass.GROUP
    ReachKind.MEMBER -> FfiReachClass.MEMBER
    ReachKind.CHANNEL -> FfiReachClass.CHANNEL
    ReachKind.FOF -> FfiReachClass.FOF
}

/** The FFI's class as [ReachKind]. */
public fun FfiReachClass.toKind(): ReachKind = when (this) {
    FfiReachClass.CONTACT -> ReachKind.CONTACT
    FfiReachClass.LINK -> ReachKind.LINK
    FfiReachClass.REPLY -> ReachKind.REPLY
    FfiReachClass.GROUP -> ReachKind.GROUP
    FfiReachClass.MEMBER -> ReachKind.MEMBER
    FfiReachClass.CHANNEL -> ReachKind.CHANNEL
    FfiReachClass.FOF -> ReachKind.FOF
}
