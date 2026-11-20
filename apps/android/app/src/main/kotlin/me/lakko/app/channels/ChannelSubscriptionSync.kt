// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlin.coroutines.cancellation.CancellationException

/**
 * One channel subscription register as this client reads it: a host-testable
 * mirror of the FFI `ChannelSubscriptionRegister` (lakko-sync
 * `channel_subscription`, docs/ux/second-device.md section 4a). The secrets
 * never print.
 */
public class ChannelRegisterView(
    /** 32 lowercase hex characters. */
    public val channelIdHex: String,
    /** The register's stamp (`0` for a value not written yet). */
    public val hlc: ULong,
    public val subscribed: Boolean,
    public val role: ChannelAccountRole,
    /** The account's ONE subscriber key seed for the channel. SECRET. */
    public val subscriberSecret: ByteArray,
    /** 0 or 32 bytes. SECRET. */
    public val readKey: ByteArray,
    /** 32 bytes for a co-publisher, else empty. */
    public val publisherGroupId: ByteArray,
    public val name: String,
    public val handle: String,
    /** ADR-0090 — 32 bytes for a co-publisher (the owner's account key its
     *  joining device verified as the `/g/` issuer), else empty. */
    public val ownerAccountKey: ByteArray = ByteArray(0),
) {
    /** Same value, stamp aside. */
    public fun sameValueAs(other: ChannelRegisterView): Boolean =
        channelIdHex == other.channelIdHex &&
            subscribed == other.subscribed &&
            role == other.role &&
            subscriberSecret.contentEquals(other.subscriberSecret) &&
            readKey.contentEquals(other.readKey) &&
            publisherGroupId.contentEquals(other.publisherGroupId) &&
            name == other.name &&
            handle == other.handle &&
            ownerAccountKey.contentEquals(other.ownerAccountKey)

    override fun toString(): String = "ChannelRegisterView($channelIdHex, subscribed=$subscribed, role=$role, <redacted>)"
}

/** What this device knows of one channel it lists. */
public class LocalChannelFacts(
    public val channelIdHex: String,
    /** [ChannelsUiState.accountRoleOf]. */
    public val role: ChannelAccountRole,
    /** This device holds the channel's PublisherGroup. */
    public val publishesHere: Boolean,
    /** The subscriber key seed this device holds, or `null`. SECRET. */
    public val subscriberSecret: ByteArray?,
    /** The read key this device holds, or `null`. SECRET. */
    public val readKey: ByteArray?,
    /** The group id a co-published channel's link conveyed, or `null`. */
    public val joinedGroupId: ByteArray?,
    /** The channel's name as the list shows it (shared, else local). */
    public val name: String,
    /** The `@name` without `@`, or "". */
    public val handle: String,
    /** When this device listed the channel (unix ms). */
    public val addedAtMs: ULong,
    /** ADR-0090 — the owner account key recorded for a co-published channel. */
    public val ownerAccountKey: ByteArray? = null,
) {
    override fun toString(): String = "LocalChannelFacts($channelIdHex, role=$role, <redacted>)"
}

/** What a reconcile pass changes ([planChannelReconcile]). */
public class ChannelReconcilePlan(
    /** Live registers of channels this device does not list: add them. */
    public val add: List<ChannelRegisterView>,
    /** Listed channels another device of the account left after this device
     *  listed them: drop them here as an unsubscribe does, without the
     *  service (the device that left told it). */
    public val drop: List<String>,
    /** Left channels this device does not list but holds seeds for (a
     *  pairing bundle carried them): forget the seeds. */
    public val forgetSeeds: List<String>,
    /** Listed channels whose live register names a subscriber key this
     *  device does not hold: adopt the register's key (one key per account
     *  and channel). */
    public val adoptKey: List<ChannelRegisterView>,
    /** Listed channels this device does not publish whose role the register
     *  states differently: record it. */
    public val setRole: List<ChannelRegisterView>,
    /** The register values this device writes, each with the stamp it was
     *  planned against ([ChannelRegisterView.hlc], `0` when none). */
    public val write: List<ChannelRegisterView>,
)

/** A register name: a display name (trimmed, no control or line-break
 *  character, at most 255 bytes) or "". */
internal fun registerName(name: String): String {
    val t = name.trim()
    if (t.isEmpty()) return ""
    if (t.any { it.isISOControl() || it == ' ' || it == ' ' }) return ""
    if (t.toByteArray(Charsets.UTF_8).size > 255) return ""
    return t
}

private val CANONICAL_HANDLE = Regex("^[a-z0-9_]{3,32}$")

/** A register handle: the canonical `@name` without `@`, or "". */
internal fun registerHandle(handle: String): String {
    val t = handle.trim().removePrefix("@")
    return if (CANONICAL_HANDLE.matches(t)) t else ""
}

/**
 * The register this device holds for [local] given what the fold holds
 * ([held], `null` when nothing). The register's key wins over a local one
 * (it is resolved by [planChannelReconcile]'s adoption); this device's
 * publishing state decides the role; a read key this device learned fills
 * in, and so do a name and an `@name` the register lacks (the publishing
 * device also replaces them). `null` when no value can be written (no key, or a
 * co-publisher with no group id).
 */
internal fun desiredChannelRegister(local: LocalChannelFacts, held: ChannelRegisterView?): ChannelRegisterView? {
    val live = held?.takeIf { it.subscribed }
    val secret = live?.subscriberSecret?.takeIf { it.size == 32 }
        ?: local.subscriberSecret?.takeIf { it.size == 32 }
        ?: return null
    val role = if (local.publishesHere || live == null) local.role else live.role
    val groupId = when (role) {
        ChannelAccountRole.CO_PUBLISHER ->
            local.joinedGroupId?.takeIf { it.size == 32 }
                ?: live?.publisherGroupId?.takeIf { it.size == 32 }
                ?: return null
        else -> ByteArray(0)
    }
    // ADR-0090 section 2.3 — a co-publisher register carries the owner's
    // account key (required by the register); an owner's or a reader's none.
    val ownerKey = when (role) {
        ChannelAccountRole.CO_PUBLISHER ->
            local.ownerAccountKey?.takeIf { it.size == 32 }
                ?: live?.ownerAccountKey?.takeIf { it.size == 32 }
                ?: return null
        else -> ByteArray(0)
    }
    val readKey = local.readKey?.takeIf { it.size == 32 }
        ?: live?.readKey?.takeIf { it.size == 32 }
        ?: ByteArray(0)
    // The name and the `@name` only fill what the register lacks, except on
    // the one device that publishes the channel: two readers holding
    // different names would otherwise overwrite each other forever.
    val heldName = live?.name.orEmpty()
    val heldHandle = live?.handle.orEmpty()
    val name = if (local.publishesHere || heldName.isEmpty()) registerName(local.name).ifEmpty { heldName } else heldName
    val handle = if (local.publishesHere || heldHandle.isEmpty()) registerHandle(local.handle).ifEmpty { heldHandle } else heldHandle
    return ChannelRegisterView(
        channelIdHex = local.channelIdHex,
        hlc = held?.hlc ?: 0uL,
        subscribed = true,
        role = role,
        subscriberSecret = secret,
        readKey = readKey,
        publisherGroupId = groupId,
        name = name,
        handle = handle,
        ownerAccountKey = ownerKey,
    )
}

/** Whether [left] (a register's leave) was written after this device
 *  listed a channel at [addedAtMs]: an HLC stamp is `wall_ms * 1000 + n`. */
internal fun leftAfterListed(left: ChannelRegisterView, addedAtMs: ULong): Boolean =
    left.hlc / 1000uL >= addedAtMs

/**
 * docs/ux/second-device.md section 4a — one reconcile of this device's
 * channels with the account's channel subscription registers:
 *
 * - a live register of a channel not listed here adds it (the other device
 *   subscribed, created it, or became a co-publisher);
 * - a leave written after this device listed the channel drops it, unless
 *   this device publishes it (only a publishing device ends its channel);
 * - a leave written before this device listed the channel is a later
 *   re-subscription here: this device writes it again;
 * - a live register whose key this device does not hold hands that key over;
 * - every listed channel with a key writes the register value it should
 *   hold (a channel listed before this build, a read key or name learned
 *   here); an identical value writes nothing;
 * - a channel in [skip] (orphaned by a key reset) is left alone.
 */
internal fun planChannelReconcile(
    listedAll: List<LocalChannelFacts>,
    registersAll: List<ChannelRegisterView>,
    seedsHeldFor: Set<String>,
    /** Channels the register leaves alone on this device (lowercase hex):
     *  orphaned by a key reset, listed or removed with `Remove from this
     *  device`. Never added, dropped, adopted or written. */
    skip: Set<String> = emptySet(),
): ChannelReconcilePlan {
    val listed = listedAll.filter { it.channelIdHex.lowercase() !in skip }
    val registers = registersAll.filter { it.channelIdHex.lowercase() !in skip }
    val byId = registers.associateBy { it.channelIdHex.lowercase() }
    val listedIds = listedAll.mapTo(HashSet()) { it.channelIdHex.lowercase() }
    val add = registers.filter { it.subscribed && it.channelIdHex.lowercase() !in listedIds }
    val forgetSeeds = registers
        .filter { !it.subscribed && it.channelIdHex.lowercase() !in listedIds && it.channelIdHex.lowercase() in seedsHeldFor }
        .map { it.channelIdHex }
    val drop = ArrayList<String>()
    val adopt = ArrayList<ChannelRegisterView>()
    val setRole = ArrayList<ChannelRegisterView>()
    val write = ArrayList<ChannelRegisterView>()
    for (local in listed) {
        val held = byId[local.channelIdHex.lowercase()]
        if (held != null && !held.subscribed) {
            if (!local.publishesHere && leftAfterListed(held, local.addedAtMs)) {
                drop += local.channelIdHex
                continue
            }
        }
        val live = held?.takeIf { it.subscribed }
        if (live != null) {
            if (live.subscriberSecret.size == 32 &&
                (local.subscriberSecret == null || !local.subscriberSecret.contentEquals(live.subscriberSecret))
            ) {
                adopt += live
            }
            if (!local.publishesHere && live.role != local.role) setRole += live
        }
        val desired = desiredChannelRegister(local, held) ?: continue
        if (live != null && live.sameValueAs(desired)) continue
        write += desired
    }
    return ChannelReconcilePlan(add, drop, forgetSeeds, adopt, setRole, write)
}

/** The FFI role of [role]. */
internal fun ffiRoleOf(role: ChannelAccountRole): uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole = when (role) {
    ChannelAccountRole.SUBSCRIBER -> uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.SUBSCRIBER
    ChannelAccountRole.OWNER -> uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.OWNER
    ChannelAccountRole.CO_PUBLISHER -> uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.CO_PUBLISHER
}

/** This client's role of [role]. */
internal fun accountRoleOfFfi(role: uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole): ChannelAccountRole = when (role) {
    uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.SUBSCRIBER -> ChannelAccountRole.SUBSCRIBER
    uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.OWNER -> ChannelAccountRole.OWNER
    uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.CO_PUBLISHER -> ChannelAccountRole.CO_PUBLISHER
}

/** [register] as this client reads it. */
internal fun viewOf(register: uniffi.lakko_ffi_uniffi.ChannelSubscriptionRegister): ChannelRegisterView =
    ChannelRegisterView(
        channelIdHex = ChannelsComposer.bytesToHex(register.channelId),
        hlc = register.hlc,
        subscribed = register.subscribed,
        role = accountRoleOfFfi(register.role),
        subscriberSecret = register.subscriberSecret,
        readKey = register.readKey,
        publisherGroupId = register.publisherGroupId,
        name = register.name,
        handle = register.handle,
        ownerAccountKey = register.ownerAccountKey,
    )

/** Where the reconcile reads and writes the account's registers; the
 *  repository in production, a fake in a host test. */
public interface ChannelRegisterPort {
    /** The account the registers are bound to (standard base64), or `null`. */
    public val boundAccount: String?

    /** Every register, leaves included; `null` while the state is not open. */
    public fun registers(): List<ChannelRegisterView>?

    /** Write [value] while the held register still carries [value]'s stamp. */
    public fun write(value: ChannelRegisterView): Boolean

    /** Write the leave of [channelIdHex]. */
    public fun leave(channelIdHex: String): Boolean
}

/** The production [ChannelRegisterPort] over the DeviceGroup sync runtime. */
public class SyncChannelRegisterPort(
    private val sync: me.lakko.app.devices.sync.DeviceGroupSyncRepository,
) : ChannelRegisterPort {
    override val boundAccount: String?
        get() = sync.boundAccountIdB64

    override fun registers(): List<ChannelRegisterView>? = sync.channelSubscriptions.value?.map(::viewOf)

    override fun write(value: ChannelRegisterView): Boolean {
        val id = ChannelsComposer.channelIdFromHex(value.channelIdHex)
        return sync.setChannelSubscription(
            channelId = id,
            role = ffiRoleOf(value.role),
            subscriberSecret = value.subscriberSecret,
            readKey = value.readKey,
            publisherGroupId = value.publisherGroupId,
            ownerAccountKey = value.ownerAccountKey,
            name = value.name,
            handle = value.handle,
            expectedHlc = value.hlc,
        )
    }

    override fun leave(channelIdHex: String): Boolean =
        sync.leaveChannelSubscription(ChannelsComposer.channelIdFromHex(channelIdHex))
}

/** The on-device stores a reconcile touches besides [ChannelsUiState]; a
 *  host test passes fakes (the read keys live in Android preferences, the
 *  subscriber key's public half comes from the native core). */
public interface ChannelReconcileStores {
    public fun readKey(account: String, channelIdHex: String): ByteArray?
    public fun putReadKey(account: String, channelIdHex: String, readKey: ByteArray)
    public fun publicKeyOf(subscriberSecret: ByteArray): ByteArray
    public fun freshSubscriberKeys(): ChannelSubscriberKeys

    /** The old key a register replaced, unsubscribed at the service in the
     *  background (best effort). */
    public fun retireOldKey(channelIdHex: String, old: ChannelSubscriberKeys)

    /** Drop a channel another device of the account left: as an
     *  unsubscribe, without the service and without telling the devices. */
    public fun dropLeft(state: ChannelsUiState, channelIdHex: String)

    /** Whether the account's subscriber key [publicKeyHex] of
     *  [channelIdHex] was registered at the channels service from this
     *  device. */
    public fun accountKeyRegistered(account: String, channelIdHex: String, publicKeyHex: String): Boolean

    /**
     * Register the account's subscriber key of a channel this device
     * publishes at the channels service as a reader (a private channel's
     * with a one-use invite key this device puts and an admission ticket),
     * and record it. `false` when it did not land (retried next pass).
     */
    public suspend fun registerAccountKey(state: ChannelsUiState, channelIdHex: String, keys: ChannelSubscriberKeys): Boolean
}

/**
 * docs/ux/second-device.md section 4a (follow-up of 2026-11-13) — the
 * channels whose account subscriber key this device registers at the service:
 * the ones it publishes (holds the PublisherGroup of), holding a key, whose
 * key it has not registered yet. The account's other devices read the
 * channel under that key, so it is a real reader key on every channel.
 */
internal fun accountKeyRegistrationsDue(
    channels: List<String>,
    publishesHere: (String) -> Boolean,
    keyOf: (String) -> ChannelSubscriberKeys?,
    registered: (channelIdHex: String, publicKeyHex: String) -> Boolean,
    skip: Set<String> = emptySet(),
): List<Pair<String, ChannelSubscriberKeys>> = channels.mapNotNull { id ->
    if (id.lowercase() in skip || !publishesHere(id)) return@mapNotNull null
    val keys = keyOf(id) ?: return@mapNotNull null
    if (registered(id, ChannelsComposer.bytesToHex(keys.publicKey))) return@mapNotNull null
    id to keys
}

/**
 * docs/ux/second-device.md section 4a — the channels of the account travel
 * between its devices through the channel subscription register. This runs
 * the reconcile ([planChannelReconcile]) against the bound [ChannelsUiState]
 * and writes the leaves ([recordLeave]).
 */
public object ChannelSubscriptionSync {
    /** 2026-11-20 — the per-channel holds of the account reader key's
     *  registration ([PublisherCommitHold]); reset at unbind. */
    internal var readerKeyRetry: PublisherCommitRetry = PublisherCommitRetry(warn = {})

    private const val TAG = "ChannelSubscriptionSync"

    private val mutex = Mutex()
    private val background = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** The application context the production stores read; set by the
     *  session shell. */
    @Volatile
    public var appContext: Context? = null

    private val _readKeysVersion = MutableStateFlow(0)

    /** Moves whenever a read key is stored, so a reconcile writes it. */
    public val readKeysVersion: StateFlow<Int> = _readKeysVersion.asStateFlow()

    /** A read key was stored ([ChannelReadKeys.put]). */
    public fun noteReadKeyStored() {
        _readKeysVersion.value = _readKeysVersion.value + 1
    }

    /** The port over the running sync runtime, or `null`. */
    public fun activePort(): ChannelRegisterPort? =
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.let(::SyncChannelRegisterPort)

    /** This device leaves [channelIdHex] (unsubscribe, delete, removal):
     *  the leave is written before the channel goes, so no reconcile adds it
     *  back. Best effort. */
    public fun recordLeave(channelIdHex: String, port: ChannelRegisterPort? = activePort()) {
        try {
            port?.leave(channelIdHex)
        } catch (e: Throwable) {
            Log.w(TAG, "channel leave not recorded: ${e.javaClass.simpleName}")
        }
    }

    /**
     * The subscriber key this device uses for [channelIdHex] when it holds
     * none: the account's key from a live register (one key per account and
     * channel, never a second one), else a fresh key. Installed in [state].
     */
    public fun keyForSubscribing(state: ChannelsUiState, channelIdHex: String): ChannelSubscriberKeys {
        state.subscriberKeysByChannel.value[channelIdHex]?.let { return it }
        val registered = try {
            activePort()?.registers()?.firstOrNull {
                it.subscribed && it.channelIdHex.equals(channelIdHex, ignoreCase = true) && it.subscriberSecret.size == 32
            }
        } catch (_: Throwable) {
            null
        }
        val keys = registered?.let { r ->
            productionStores()?.let { stores ->
                ChannelSubscriberKeys(secret = r.subscriberSecret.copyOf(), publicKey = stores.publicKeyOf(r.subscriberSecret))
            }
        } ?: ChannelsComposer.freshSubscriberKeys()
        state.setSubscriberKeys(channelIdHex, keys)
        return keys
    }

    /** One reconcile; a no-op until the state, the registers and the
     *  publisher keys are all bound to the same account. Never throws. */
    public suspend fun reconcile(
        state: ChannelsUiState,
        port: ChannelRegisterPort? = activePort(),
        stores: ChannelReconcileStores? = productionStores(),
    ) {
        if (port == null || stores == null) return
        mutex.withLock {
            try {
                reconcileLocked(state, port, stores)
                registerAccountKeys(state, stores)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "channel reconcile failed: ${e.javaClass.simpleName}")
            }
        }
    }

    /** Register at the service, once, the account key of every channel this
     *  device publishes ([accountKeyRegistrationsDue]). Best effort per
     *  channel. */
    internal suspend fun registerAccountKeys(state: ChannelsUiState, stores: ChannelReconcileStores) {
        val account = state.boundAccount ?: return
        if (!state.publisherKeysReady.value) return
        val due = accountKeyRegistrationsDue(
            channels = state.channels.value.map { it.id },
            publishesHere = state::publishesHere,
            keyOf = { state.subscriberKeysByChannel.value[it] },
            registered = { id, pubHex -> stores.accountKeyRegistered(account, id, pubHex) },
            skip = state.registerSkipSet(),
        )
        for ((id, keys) in due) {
            // 2026-11-20 — a refused or failing registration waits out its
            // hold (a refusal: until the next binding) instead of being tried
            // at every reconcile.
            if (!readerKeyRetry.mayAttempt(account, id)) continue
            try {
                if (stores.registerAccountKey(state, id, keys)) Log.i(TAG, "the account's reader key of a channel is registered")
                readerKeyRetry.noteSuccess(account, id)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                val hold = readerKeyRetry.noteFailure(account, id, e)
                Log.w(
                    TAG,
                    "the account's reader key was not registered yet (${PublisherCommitFailure.logLabel(e)}); " +
                        if (hold.refused) "not tried again until the next start" else "tried again after a wait",
                )
            }
        }
    }

    internal fun reconcileLocked(state: ChannelsUiState, port: ChannelRegisterPort, stores: ChannelReconcileStores) {
        val account = state.boundAccount ?: return
        val portAccount = port.boundAccount ?: return
        if (canonical(account) != canonical(portAccount)) return
        if (!state.publisherKeysReady.value) return
        val registers = port.registers() ?: return
        // A channel this device publishes always carries a subscriber key in
        // the register (the account's one key); minted here when it has none.
        for (c in state.channels.value) {
            if (state.publishesHere(c.id) && state.subscriberKeysByChannel.value[c.id] == null &&
                registers.none { it.channelIdHex.equals(c.id, ignoreCase = true) && it.subscribed }
            ) {
                state.setSubscriberKeys(c.id, stores.freshSubscriberKeys())
            }
        }
        val listed = state.channels.value.map { c ->
            LocalChannelFacts(
                channelIdHex = c.id,
                role = state.accountRoleOf(c.id),
                publishesHere = state.publishesHere(c.id),
                subscriberSecret = state.subscriberKeysByChannel.value[c.id]?.secret,
                readKey = stores.readKey(account, c.id),
                joinedGroupId = state.joinedGroupIdOf(c.id),
                name = c.sharedName.ifBlank { c.localName },
                handle = c.handle,
                addedAtMs = c.addedAtMs,
                ownerAccountKey = state.coPublished?.ownerKeyOf(c.id),
            )
        }
        val plan = planChannelReconcile(
            listed,
            registers,
            state.subscribedSeedsByChannel.value.keys,
            skip = state.registerSkipSet(),
        )
        for (id in plan.drop) {
            Log.i(TAG, "a channel left on another device goes here too")
            stores.dropLeft(state, id)
        }
        for (id in plan.forgetSeeds) state.clearSubscriberKeys(id)
        for (r in plan.add) {
            install(state, account, r, stores)
            state.addChannel(
                ChannelHandle(
                    id = r.channelIdHex,
                    localName = r.name.ifBlank { r.handle },
                    localDescription = "",
                    addedAtMs = System.currentTimeMillis().toULong(),
                    handle = r.handle,
                    role = r.role,
                ),
            )
        }
        for (r in plan.adoptKey) {
            val old = state.subscriberKeysByChannel.value[r.channelIdHex]
            install(state, account, r, stores)
            if (old != null && !old.secret.contentEquals(r.subscriberSecret)) stores.retireOldKey(r.channelIdHex, old)
        }
        for (r in plan.setRole) {
            state.setAccountRole(r.channelIdHex, r.role)
            markJoined(state, r)
        }
        for (r in plan.write) {
            if (!port.write(r)) continue
            Log.i(TAG, "channel subscription written")
        }
    }

    /** Install what [r] carries for this device: the shared subscriber key
     *  (never a fresh one), the read key, a co-publisher's group mark. */
    private fun install(state: ChannelsUiState, account: String, r: ChannelRegisterView, stores: ChannelReconcileStores) {
        if (r.subscriberSecret.size == 32) {
            val held = state.subscriberKeysByChannel.value[r.channelIdHex]
            if (held == null || !held.secret.contentEquals(r.subscriberSecret)) {
                state.setSubscriberKeys(
                    r.channelIdHex,
                    ChannelSubscriberKeys(secret = r.subscriberSecret.copyOf(), publicKey = stores.publicKeyOf(r.subscriberSecret)),
                )
            }
        }
        if (r.readKey.size == 32 && stores.readKey(account, r.channelIdHex) == null) {
            stores.putReadKey(account, r.channelIdHex, r.readKey)
        }
        markJoined(state, r)
    }

    private fun markJoined(state: ChannelsUiState, r: ChannelRegisterView) {
        if (r.role != ChannelAccountRole.CO_PUBLISHER || r.publisherGroupId.size != 32) return
        // ADR-0090 — the owner key the joining device recorded: this device
        // takes a PublisherGroup Welcome only from that account's devices.
        if (r.ownerAccountKey.size == 32 && state.coPublished?.ownerKeyOf(r.channelIdHex) == null) {
            state.coPublished?.markOwner(r.channelIdHex, r.ownerAccountKey)
        }
        if (state.joinedGroupIdOf(r.channelIdHex) != null) return
        state.coPublished?.markChannel(ChannelsComposer.bytesToHex(r.publisherGroupId), r.channelIdHex)
    }

    private fun canonical(account: String): String =
        me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(account)

    /** The production stores, or `null` before the session set the context. */
    public fun productionStores(): ChannelReconcileStores? {
        val context = appContext ?: return null
        return object : ChannelReconcileStores {
            override fun readKey(account: String, channelIdHex: String): ByteArray? =
                ChannelReadKeys.get(context, account, channelIdHex)

            override fun putReadKey(account: String, channelIdHex: String, readKey: ByteArray) =
                ChannelReadKeys.put(context, account, channelIdHex, readKey)

            override fun publicKeyOf(subscriberSecret: ByteArray): ByteArray =
                uniffi.lakko_ffi_uniffi.ChannelSubscriberKeypair.fromSecret(subscriberSecret).use { it.`public`() }

            override fun freshSubscriberKeys(): ChannelSubscriberKeys = ChannelsComposer.freshSubscriberKeys()

            override fun retireOldKey(channelIdHex: String, old: ChannelSubscriberKeys) {
                val keys = ChannelSubscriberKeys(secret = old.secret.copyOf(), publicKey = old.publicKey.copyOf())
                background.launch {
                    try {
                        ChannelsClient().use { client -> ChannelUnsubscribe.tellServer(client, channelIdHex, keys) }
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        Log.w(TAG, "a replaced subscriber key was not unsubscribed: ${e.javaClass.simpleName}")
                    } finally {
                        keys.secret.fill(0)
                    }
                }
            }

            override fun dropLeft(state: ChannelsUiState, channelIdHex: String) {
                ChannelUnsubscribe.leave(
                    state,
                    channelIdHex,
                    ChannelsClient.DEFAULT_BASE_URL,
                    tellDevices = false,
                    tellServer = false,
                )
            }

            override fun accountKeyRegistered(account: String, channelIdHex: String, publicKeyHex: String): Boolean =
                ChannelAccountReaderKey.isRegistered(context, account, channelIdHex, publicKeyHex)

            override suspend fun registerAccountKey(
                state: ChannelsUiState,
                channelIdHex: String,
                keys: ChannelSubscriberKeys,
            ): Boolean = ChannelsClient().use { client ->
                ChannelAccountReaderKey.register(context, state, client, channelIdHex, keys)
            }
        }
    }
}

/** [flow] debounced by [ms] (the reconcile trigger). */
@OptIn(kotlinx.coroutines.FlowPreview::class)
public fun <T> debounced(flow: kotlinx.coroutines.flow.Flow<T>, ms: Long): kotlinx.coroutines.flow.Flow<T> =
    flow.debounce(ms)
