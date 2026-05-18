// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import android.content.Context
import android.util.Log
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.auth.base64Encode
import me.lakko.app.devices.DeviceGroupLifecycle
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import uniffi.lakko_ffi_uniffi.DeviceGroup
import uniffi.lakko_ffi_uniffi.DeviceGroupBody
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.parseDevicegroupPayload

/**
 * Cross-device sync runtime on Android (Phase 2a3e2b).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/sync/DeviceGroupSyncContext.tsx`
 *   * `apps/ios/Lakko/Devices/Sync/DeviceGroupSyncRepository.swift`
 *
 * Holds the per-session DeviceGroup handle + four reactive
 * state maps (drafts / flags / readCursors / order) the UI
 * observes via [StateFlow], plus a background poll loop that
 * drains the DeviceGroup DS journal and a set of typed send
 * paths for the outbound side.
 *
 * Lifecycle:
 *
 *   * [bootstrap] (idempotent for the same `accountId`) —
 *     loads the DeviceGroup handle from the existing snapshot
 *     or bootstraps a founder leaf, kicks off the poll loop
 *     on the repository's [scope].
 *   * [clear] — cancels the poll loop coroutine, drops the
 *     handle, clears every reactive map. Called from
 *     `AuthRepository.logout` / `AuthRepository.forgetDevice`.
 */
public class DeviceGroupSyncRepository(
    private val applicationContext: Context,
) {

    // ------------------------------------------------------------
    // Reactive state
    // ------------------------------------------------------------

    private val mutableReady: MutableStateFlow<Boolean> =
        MutableStateFlow(false)
    public val ready: StateFlow<Boolean> = mutableReady.asStateFlow()

    private val mutableDrafts: MutableStateFlow<Map<String, DraftState>> =
        MutableStateFlow(emptyMap())
    public val drafts: StateFlow<Map<String, DraftState>> =
        mutableDrafts.asStateFlow()

    private val mutableFlags: MutableStateFlow<Map<String, FlagsState>> =
        MutableStateFlow(emptyMap())
    public val flags: StateFlow<Map<String, FlagsState>> =
        mutableFlags.asStateFlow()

    private val mutableReadCursors:
        MutableStateFlow<Map<String, ReadCursorState>> =
        MutableStateFlow(emptyMap())
    public val readCursors: StateFlow<Map<String, ReadCursorState>> =
        mutableReadCursors.asStateFlow()

    private val mutableOrder: MutableStateFlow<OrderState?> =
        MutableStateFlow(null)
    public val order: StateFlow<OrderState?> = mutableOrder.asStateFlow()

    // ------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------

    private val scope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val identity = AtomicReference<Identity?>(null)
    private val group = AtomicReference<DeviceGroup?>(null)
    private val activeAccountIdB64 = AtomicReference<String?>(null)
    @Volatile private var pollJob: Job? = null

    // ------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------

    /**
     * Bootstrap the cross-device sync runtime. Idempotent for
     * the same account. Called from `MainActivity` /
     * `SessionRoot` whenever a fresh [IdentitySecrets] value
     * lands.
     */
    public fun bootstrap(secrets: IdentitySecrets) {
        val accountIdB64 = base64Encode(secrets.accountId)
        val existing = activeAccountIdB64.get()
        if (existing == accountIdB64) return
        if (existing != null) {
            cancelPollAndDropHandles()
        }
        activeAccountIdB64.set(accountIdB64)

        val seed = try {
            me.lakko.app.auth.DeviceIdentityStore.ensureDeviceIdentitySeed(
                context = applicationContext,
                accountIdB64 = accountIdB64,
            )
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "ensureDeviceIdentitySeed failed", cause)
            activeAccountIdB64.set(null)
            return
        }

        val ensured = try {
            DeviceGroupLifecycle.ensureDeviceGroup(
                context = applicationContext,
                accountIdBytes = secrets.accountId,
                accountIdB64 = accountIdB64,
                deviceIdentitySeed = seed,
            )
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "ensureDeviceGroup failed", cause)
            activeAccountIdB64.set(null)
            return
        }
        seed.fill(0)

        identity.set(ensured.identity)
        group.set(ensured.group)
        mutableReady.value = true

        val groupId = try {
            ensured.group.groupId()
        } catch (cause: Throwable) {
            Log.w(LOG_TAG, "DeviceGroup.groupId failed", cause)
            return
        }
        pollJob = scope.launch {
            runPollLoop(
                accountIdB64 = accountIdB64,
                groupId = groupId,
            )
        }
    }

    /**
     * Tear down. Cancels the poll loop, closes the FFI
     * handles, clears every reactive map. Idempotent.
     */
    public fun clear() {
        cancelPollAndDropHandles()
        activeAccountIdB64.set(null)
        mutableReady.value = false
        mutableDrafts.value = emptyMap()
        mutableFlags.value = emptyMap()
        mutableReadCursors.value = emptyMap()
        mutableOrder.value = null
    }

    /** Permanently destroy the repository — cancels the
     *  per-instance coroutine scope. After this, [bootstrap]
     *  no longer fires the poll loop. */
    public fun close() {
        clear()
        scope.cancel()
    }

    /** Lookup helper: current draft for `groupId` (raw bytes)
     *  or `null`. */
    public fun draftFor(groupId: ByteArray): DraftState? =
        drafts.value[groupIdHexOf(groupId)]

    /** Lookup helper: current flags or `null`. */
    public fun flagsFor(groupId: ByteArray): FlagsState? =
        flags.value[groupIdHexOf(groupId)]

    /** Lookup helper: current read cursor or `null`. */
    public fun readCursorFor(groupId: ByteArray): ReadCursorState? =
        readCursors.value[groupIdHexOf(groupId)]

    // ------------------------------------------------------------
    // Outbound send paths
    // ------------------------------------------------------------

    /** Compose + publish a `Draft` payload. Empty `text`
     *  clears the receivers' draft for the same group. */
    public suspend fun setDraft(
        groupId: ByteArray,
        text: String,
        replyToId: ByteArray = ByteArray(0),
    ) {
        val hlc = Hlc.mint()
        val body = DeviceGroupBody.Draft(
            groupId = groupId,
            text = text,
            replyToId = replyToId,
        )
        sendBody(body, hlc)
        val key = groupIdHexOf(groupId)
        mutableDrafts.value = mutableDrafts.value + (key to DraftState(
            text = text,
            replyToId = replyToId,
            updatedAtHlc = hlc,
        ))
    }

    /** Compose + publish a `Flags` payload. */
    public suspend fun setFlags(
        groupId: ByteArray,
        pinned: Boolean,
        muted: Boolean,
        archived: Boolean,
        unreadOverride: Boolean,
    ) {
        val hlc = Hlc.mint()
        val body = DeviceGroupBody.Flags(
            groupId = groupId,
            pinned = pinned,
            muted = muted,
            archived = archived,
            unreadOverride = unreadOverride,
        )
        sendBody(body, hlc)
        val key = groupIdHexOf(groupId)
        mutableFlags.value = mutableFlags.value + (key to FlagsState(
            pinned = pinned,
            muted = muted,
            archived = archived,
            unreadOverride = unreadOverride,
            updatedAtHlc = hlc,
        ))
    }

    /** Compose + publish a `ReadCursor` payload. */
    public suspend fun setReadCursor(
        groupId: ByteArray,
        upToMessageId: ByteArray,
        readAtMs: ULong,
    ) {
        val hlc = Hlc.mint()
        val body = DeviceGroupBody.ReadCursor(
            groupId = groupId,
            upToMessageId = upToMessageId,
            readAtMs = readAtMs,
        )
        sendBody(body, hlc)
        val key = groupIdHexOf(groupId)
        mutableReadCursors.value = mutableReadCursors.value + (key to ReadCursorState(
            upToMessageId = upToMessageId,
            readAtMs = readAtMs,
            updatedAtHlc = hlc,
        ))
    }

    /** Compose + publish an `Order` payload. */
    public suspend fun setOrder(groupIds: List<ByteArray>) {
        val hlc = Hlc.mint()
        val body = DeviceGroupBody.Order(groupIds = groupIds)
        sendBody(body, hlc)
        mutableOrder.value = OrderState(
            groupIds = groupIds,
            updatedAtHlc = hlc,
        )
    }

    private suspend fun sendBody(body: DeviceGroupBody, hlc: ULong) {
        val g = group.get() ?: error("DeviceGroupSync not bootstrapped")
        val id = identity.get() ?: error("DeviceGroupSync not bootstrapped")
        val accountIdB64 = activeAccountIdB64.get()
            ?: error("DeviceGroupSync not bootstrapped")
        val wireBytes = g.createDevicegroupMessage(id, body, hlc)
        val groupId = g.groupId()
        DsClient().use { ds ->
            ds.publishGroup(groupId, wireBytes)
        }
        DeviceGroupLifecycle.persistSnapshot(
            context = applicationContext,
            identity = id,
            accountIdB64 = accountIdB64,
        )
    }

    // ------------------------------------------------------------
    // Poll loop
    // ------------------------------------------------------------

    private suspend fun runPollLoop(
        accountIdB64: String,
        groupId: ByteArray,
    ) {
        var cursor = 0L
        DsClient().use { ds ->
            while (currentCoroutineContextActive()) {
                val result = try {
                    ds.fetchGroup(
                        groupId = groupId,
                        after = cursor,
                        limit = null,
                        waitSecs = 25,
                    )
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (http: DsHttpError) {
                    if (http.status == 404) continue
                    Log.w(LOG_TAG, "poll fetch http", http)
                    delay(5000)
                    continue
                } catch (_: DsNetworkError) {
                    delay(2000)
                    continue
                } catch (cause: Throwable) {
                    Log.w(LOG_TAG, "poll fetch failed", cause)
                    delay(5000)
                    continue
                }

                var mutated = false
                for (message in result.messages) {
                    cursor = message.id
                    val g = group.get() ?: return
                    val id = identity.get() ?: return
                    val processed: ProcessedMessage = try {
                        g.processIncoming(id, message.payload)
                    } catch (cause: Throwable) {
                        Log.w(LOG_TAG, "processIncoming failed at ${message.id}", cause)
                        continue
                    }
                    mutated = true
                    if (processed is ProcessedMessage.Application) {
                        try {
                            val parsed = parseDevicegroupPayload(
                                processed.`data`,
                            )
                            applyParsed(parsed)
                        } catch (cause: Throwable) {
                            Log.w(LOG_TAG, "parseDevicegroupPayload failed", cause)
                        }
                    }
                    // Commit / Proposal / ExternalJoinProposal —
                    // no application-state mutation.
                }

                if (mutated) {
                    val id = identity.get()
                    if (id != null) {
                        DeviceGroupLifecycle.persistSnapshot(
                            context = applicationContext,
                            identity = id,
                            accountIdB64 = accountIdB64,
                        )
                    }
                }
            }
        }
    }

    private fun applyParsed(
        parsed: uniffi.lakko_ffi_uniffi.ParsedDeviceGroupPayload,
    ) {
        val hlc = parsed.hlc
        when (val body = parsed.body) {
            is DeviceGroupBody.ReadCursor -> {
                val key = groupIdHexOf(body.groupId)
                val cur = mutableReadCursors.value[key]
                if (shouldApply(hlc, cur?.updatedAtHlc)) {
                    mutableReadCursors.value = mutableReadCursors.value + (
                        key to ReadCursorState(
                            upToMessageId = body.upToMessageId,
                            readAtMs = body.readAtMs,
                            updatedAtHlc = hlc,
                        )
                    )
                }
            }
            is DeviceGroupBody.Draft -> {
                val key = groupIdHexOf(body.groupId)
                val cur = mutableDrafts.value[key]
                if (shouldApply(hlc, cur?.updatedAtHlc)) {
                    mutableDrafts.value = mutableDrafts.value + (
                        key to DraftState(
                            text = body.text,
                            replyToId = body.replyToId,
                            updatedAtHlc = hlc,
                        )
                    )
                }
            }
            is DeviceGroupBody.Flags -> {
                val key = groupIdHexOf(body.groupId)
                val cur = mutableFlags.value[key]
                if (shouldApply(hlc, cur?.updatedAtHlc)) {
                    mutableFlags.value = mutableFlags.value + (
                        key to FlagsState(
                            pinned = body.pinned,
                            muted = body.muted,
                            archived = body.archived,
                            unreadOverride = body.unreadOverride,
                            updatedAtHlc = hlc,
                        )
                    )
                }
            }
            is DeviceGroupBody.Order -> {
                val cur = mutableOrder.value
                if (shouldApply(hlc, cur?.updatedAtHlc)) {
                    mutableOrder.value = OrderState(
                        groupIds = body.groupIds,
                        updatedAtHlc = hlc,
                    )
                }
            }
        }
    }

    private fun cancelPollAndDropHandles() {
        pollJob?.cancel()
        pollJob = null
        // Drop wasm handle references. We don't `.close()` them
        // — the UniFFI Object pattern means closing here while
        // some other call path still holds a reference would
        // crash. Letting them go out of scope is enough; the
        // JNA finalizer reclaims the Rust-side Arc.
        identity.set(null)
        group.set(null)
    }

    private companion object {
        const val LOG_TAG = "DeviceGroupSync"
    }
}

/**
 * Tiny wrapper around `currentCoroutineContext().isActive`
 * that doesn't require importing
 * `kotlinx.coroutines.currentCoroutineContext` at every call
 * site — same shape as the WelcomeCourier helper.
 */
private suspend fun currentCoroutineContextActive(): Boolean =
    kotlinx.coroutines.currentCoroutineContext().isActive
