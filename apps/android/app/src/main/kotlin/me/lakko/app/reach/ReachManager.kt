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
) {
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
     *  with the account unless [forget] is set. */
    public fun clear(forget: Boolean = false) {
        val old = binding.getAndSet(null)
        if (forget && old != null) store.delete(old.accountIdB64)
        old?.accountSeed?.fill(0)
        pendingDeviceSync.clear()
        doc = ReachDoc()
        mutableVisibility.value = null
        mutableStatus.value = ReachApplyStatus.APPLIED
    }

    /** The bound account's signature public key, or `null`. */
    public fun ownAccountKey(): ByteArray? = binding.get()?.accountKey?.copyOf()

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

    /** The Visibility and ReachState envelopes in force here, verbatim with
     *  their original stamps: what this device sends a device it admits to
     *  the DeviceGroup (ADR-0060 §7). Empty for what was never set. */
    public fun deviceSyncEnvelopes(): List<ByteArray> =
        if (binding.get() == null) emptyList() else deviceSyncEnvelopesToSend(doc.visibilityHex, doc.reachStateHex)

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
        val setting = mutableVisibility.value ?: uniffi.lakko_ffi_uniffi.defaultVisibilitySetting()
        val inputs = inputsFor(doc, setting)
        val desired = desiredReachKeys(inputs, nowMs).associateBy { key ->
            reachHex(publicKeyOf(derive(b, key.slot, key.version).seed))
        }
        val registered = doc.registered.mapNotNull { r ->
            val kind = runCatching { ReachKind.valueOf(r.kind) }.getOrNull() ?: return@mapNotNull null
            r.pkHex to RegisteredReachKey(ReachSlot(kind, r.scopeHex), r.version, r.expiresAtMs)
        }.toMap()
        val diff = diffReachKeys(desired, registered)
        val wantOpen = setting.mode == FfiVisibilityMode.EVERYONE
        val openChange = if (doc.registeredOpen == wantOpen) null else wantOpen
        // Expired replies drop out of the document once the diff removed them.
        val liveReplies = doc.replies.filter { it.expiresAtMs > nowMs }
        if (diff.isEmpty && openChange == null) {
            if (liveReplies.size != doc.replies.size) {
                doc = doc.copy(replies = liveReplies)
                store.save(b.accountIdB64, doc)
            }
            mutableStatus.value = ReachApplyStatus.APPLIED
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
            doc = doc.copy(
                registered = desired.map { (pk, key) ->
                    StoredRegistered(pk, key.slot.kind.name, key.slot.scopeHex, key.version, key.expiresAtMs)
                },
                registeredOpen = wantOpen,
                replies = liveReplies,
            )
            store.save(b.accountIdB64, doc)
            mutableStatus.value = ReachApplyStatus.APPLIED
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
            doc = doc.copy(visibilityHex = reachHex(envelope))
            store.save(b.accountIdB64, doc)
            syncLocked(b, System.currentTimeMillis())
        }
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
        scope.launch {
            lock.withLock {
                val existing = doc.held.firstOrNull {
                    it.ownerKeyHex == ownerHex && it.kind == kind.name && it.scopeHex == scopeKey
                }
                if (existing != null && existing.grantHex == reachHex(wire)) return@withLock
                if (existing != null) {
                    val old = runCatching {
                        uniffi.lakko_ffi_uniffi.decodeReachGrant(reachUnhex(existing.grantHex)!!)
                    }.getOrNull()
                    if (old != null && old.version > grant.version) return@withLock
                }
                doc = doc.copy(
                    held = doc.held.filterNot {
                        it.ownerKeyHex == ownerHex && it.kind == kind.name && it.scopeHex == scopeKey
                    } + StoredHeld(ownerHex, kind.name, scopeKey, reachHex(wire)),
                )
                store.save(b.accountIdB64, doc)
            }
        }
    }

    /** Drop every grant held for [ownerKey] (a contact removed). */
    public fun forgetHeld(ownerKey: ByteArray) {
        val b = binding.get() ?: return
        val ownerHex = reachHex(ownerKey)
        scope.launch {
            lock.withLock {
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
        val b = binding.get() ?: return null
        if (ownerKey.contentEquals(b.accountKey)) {
            val own = grant(ReachKind.CONTACT) ?: return null
            return DsReachAuth(reachSigner(own.seed))
        }
        val held = heldFor(ownerKey)
        val pick = pickWriteGrant(held.map { it.first }, purpose, groupHex?.lowercase())
        if (pick != null) {
            val grant = held.first { it.first == pick }.second
            return DsReachAuth(reachSigner(grant.seed))
        }
        if (purpose == WritePurpose.WELCOME && groupHex != null) {
            delegationFor(groupHex, ownerKey)?.let { header ->
                return DsReachAuth(reachSigner(b.accountSeed.copyOf()), delegationHeader = header)
            }
        }
        return null
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
                doc = doc.copy(
                    delegations = doc.delegations.filterNot {
                        it.groupHex == entry.groupHex && it.senderKeyHex == entry.senderKeyHex &&
                            it.addedAccountKeyHex == entry.addedAccountKeyHex
                    } + entry,
                )
                store.save(b.accountIdB64, doc)
            }
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

        /** How long a committer delegation lives: the 24-hour maximum. */
        public const val DELEGATION_TTL_MS: Long = 24L * 60 * 60 * 1000

        private const val DELEGATION_NONCE_LEN = 16

        @Volatile
        private var instance: ReachManager? = null

        /** The process-wide manager. */
        public fun get(context: Context): ReachManager =
            instance ?: synchronized(this) {
                instance ?: ReachManager(EncryptedReachDocStore(context)).also { instance = it }
            }

        /** The manager if one was created, else `null` (background readers). */
        public fun peek(): ReachManager? = instance
    }
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
