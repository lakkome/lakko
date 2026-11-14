// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.withTimeoutOrNull
import me.lakko.app.storage.VaultClient
import me.lakko.app.storage.VaultRequestSigner
import uniffi.lakko_ffi_uniffi.FolderConversationKind
import uniffi.lakko_ffi_uniffi.FolderConversationRef
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.VaultConversationHead
import uniffi.lakko_ffi_uniffi.VaultConversationKind
import uniffi.lakko_ffi_uniffi.VaultKeys
import uniffi.lakko_ffi_uniffi.VaultMembership
import uniffi.lakko_ffi_uniffi.VaultReentryEpoch
import uniffi.lakko_ffi_uniffi.VaultRef

/*
 * ADR-0064 section 7, amendment of 2026-11-14 — an owned channel's vault
 * head. Every owner device that holds the channel's PublisherGroup keeps the
 * two newest GroupInfos (each with that epoch's Delivery Service access seed)
 * in the vault at the channel's conversation ref under the owner account's
 * vault root: before it sends a PublisherGroup commit (the epoch the staged
 * commit moves to) and after it applies another owner device's commit (the
 * current one). An owner device that holds no PublisherGroup reads the head
 * and enters by an external commit, newest saved epoch first.
 *
 * The decisions (which epochs to keep, the read-merge-write loop, the order
 * entries are tried in, who enters) are written against small interfaces so
 * the host suite runs them; [ChannelVaultHeads] binds them to the core and
 * the storage service.
 */

/** Re-entry material of one epoch. [dsAccessSeed] is secret: never logged. */
public class ReentryEpoch(public val epoch: Long, public val dsAccessSeed: ByteArray, public val groupInfo: ByteArray) {
    override fun toString(): String = "ReentryEpoch(epoch=$epoch)"
}

/** How many epochs a head keeps (the amendment, item 2). */
public const val REENTRY_EPOCHS_KEPT: Int = 2

/**
 * The epochs a head keeps after a write: those of [held] and [incoming],
 * one per epoch ([incoming] replaces a held one of the same epoch), the
 * [keep] newest, newest first. Pure.
 */
public fun mergeReentry(
    held: List<ReentryEpoch>,
    incoming: List<ReentryEpoch>,
    keep: Int = REENTRY_EPOCHS_KEPT,
): List<ReentryEpoch> {
    val byEpoch = HashMap<Long, ReentryEpoch>()
    for (r in held) byEpoch[r.epoch] = r
    for (r in incoming) byEpoch[r.epoch] = r
    return byEpoch.values.sortedByDescending { it.epoch }.take(keep)
}

/** A head as opened: its re-entry epochs (the only field any client reads;
 *  a write rebuilds the head with these carried over and nothing else). */
public class OpenedHead(public val reentry: List<ReentryEpoch>)

/** One vault's head on the storage service. */
public interface VaultHeadStore {
    public suspend fun read(): VaultClient.HeadRead

    public suspend fun write(version: Long, sealed: ByteArray): VaultClient.HeadWrite

    /** Claim the vault (one `vault-claim` token). */
    public suspend fun claim()
}

/** Seal and open one vault's head. */
public interface VaultHeadCodec {
    /** Throws when the bytes do not open as this vault's head at [version]. */
    public fun open(version: Long, sealed: ByteArray): OpenedHead

    public fun seal(version: Long, reentry: List<ReentryEpoch>): ByteArray
}

/** How many reads and writes one head write takes at most (two conflicts and
 *  a claim). Chosen, not measured. */
public const val HEAD_WRITE_ATTEMPTS: Int = 3

/**
 * Write [incoming] into the head: read it (claiming the vault when nobody
 * has), keep the two newest epochs of both, write at the next version; a
 * `409` (another device wrote that version) reads, merges and writes again.
 * Returns whether it was written within [HEAD_WRITE_ATTEMPTS]; `false`, with
 * no write, when every incoming epoch is older than the two the head keeps. Network
 * failures throw; the caller never lets them reach a commit.
 */
public suspend fun writeVaultHead(
    store: VaultHeadStore,
    codec: VaultHeadCodec,
    incoming: List<ReentryEpoch>,
    /** Security pass — decision A re-founded the group: the head holds
     *  [incoming] alone (the dead group's higher epochs are dropped, never
     *  merged), still at the current version + 1. */
    replace: Boolean = false,
): Boolean {
    var claimed = false
    repeat(HEAD_WRITE_ATTEMPTS) {
        var version = 0L
        var base: OpenedHead? = null
        when (val read = store.read()) {
            is VaultClient.HeadRead.Found -> {
                version = read.version
                // A head this key cannot open is written over: it is ours.
                base = runCatching { codec.open(read.version, read.sealed) }.getOrNull()
            }
            VaultClient.HeadRead.Missing -> Unit
            VaultClient.HeadRead.Refused -> {
                if (claimed) return false
                store.claim()
                claimed = true
            }
        }
        val merged = if (replace) mergeReentry(emptyList(), incoming) else mergeReentry(base?.reentry.orEmpty(), incoming)
        // Every incoming epoch is older than the saved ones: nothing of it
        // would be kept, so nothing is written (and nothing reported as written).
        if (merged.none { m -> incoming.any { it === m } }) return false
        val next = version + 1
        when (store.write(next, codec.seal(next, merged))) {
            VaultClient.HeadWrite.WRITTEN -> return true
            VaultClient.HeadWrite.CONFLICT -> Unit
            VaultClient.HeadWrite.REFUSED -> {
                if (claimed) return false
                store.claim()
                claimed = true
            }
        }
    }
    return false
}

/** The saved re-entry epochs of the head, empty when there is none (never
 *  claimed, nothing written, or not openable). A network failure throws: the
 *  caller cannot tell "no head" from "not reached" then. */
public suspend fun readVaultReentry(store: VaultHeadStore, codec: VaultHeadCodec): List<ReentryEpoch> =
    when (val read = store.read()) {
        is VaultClient.HeadRead.Found -> runCatching { codec.open(read.version, read.sealed).reentry }.getOrDefault(emptyList())
        VaultClient.HeadRead.Missing, VaultClient.HeadRead.Refused -> emptyList()
    }

/** How one entry from a saved epoch ended. */
public enum class EntryAttempt {
    /** The Delivery Service took it; the device holds the group. */
    ENTERED,

    /** Not taken (`409`, a refused proof) or refused by the core (a stale or
     *  foreign GroupInfo): try the older epoch. */
    NOT_TAKEN,
}

/**
 * The amendment, item 4 — try the saved epochs newest first; the first
 * [EntryAttempt.ENTERED] ends it (`true`); none, or every one not taken, is
 * `false` (the device waits, or decision A runs). A throw of [attempt] (no
 * answer) ends it and propagates: the recorded entry is resumed later.
 */
public suspend fun enterNewestFirst(
    saved: List<ReentryEpoch>,
    attempt: suspend (ReentryEpoch) -> EntryAttempt,
): Boolean {
    for (r in saved.sortedByDescending { it.epoch }) {
        if (attempt(r) == EntryAttempt.ENTERED) return true
    }
    return false
}

/**
 * The amendment, items 1 and 4 — whether this device tries to enter a
 * channel's PublisherGroup from the vault: only for a channel the account
 * OWNS (never a co-published one: an owner device admits those), still
 * subscribed, whose group this device does not hold (no keys, no group
 * state), not orphaned, and with no suspected reset (a suspended device
 * holds the group).
 */
public fun entryWanted(
    ownerRole: Boolean,
    subscribed: Boolean,
    holdsKeys: Boolean,
    holdsGroup: Boolean,
    suspended: Boolean,
    orphaned: Boolean,
): Boolean = ownerRole && subscribed && !holdsKeys && !holdsGroup && !suspended && !orphaned

/** The amendment, item 3 — after a journal read, whether an owner device
 *  writes the head with the current GroupInfo: it applied a commit (another
 *  owner device's: a co-publisher never commits) and holds no commit of its
 *  own waiting for the Delivery Service. */
public fun headWriteAfterPoll(owner: Boolean, epochMoved: Boolean, hasStagedCommit: Boolean): Boolean =
    owner && epochMoved && !hasStagedCommit

/** Security pass on the channel entry — whether a journal read added a leaf
 *  of the owner account ([after] holds a key [before] did not): an entry (or
 *  an admission) the reconcile must look at now. Pure. */
public fun ownLeafAdded(before: List<ByteArray>, after: List<ByteArray>): Boolean =
    after.any { a -> before.none { it.contentEquals(a) } }

/** Security pass on the channel entry — whether the pass runs the own-device
 *  reconcile right after its journal read: an owner device ([before] is its
 *  owner-account leaves before the read, `null` for a co-publisher or no
 *  read) that applied a commit adding a leaf of the owner account. Pure. */
public fun reconcileAfterRead(before: List<ByteArray>?, epochMoved: Boolean, after: () -> List<ByteArray>): Boolean =
    before != null && epochMoved && ownLeafAdded(before, after())

/** The longest the vault deletes before an account deletion may take
 *  (chosen, not measured): a slow storage service never holds the deletion. */
public const val VAULT_DELETE_TIMEOUT_MS: Long = 15_000L

/**
 * ADR-0064 section 1 — the client deletes its vaults: [delete] each of
 * [channelIds] in turn, a failure of one logged by [warn] and passed over,
 * the whole bounded by [timeoutMs]. Returns how many were deleted. Never
 * throws but for cancellation of the caller.
 */
public suspend fun deleteVaultsBounded(
    channelIds: List<ByteArray>,
    timeoutMs: Long = VAULT_DELETE_TIMEOUT_MS,
    warn: (String) -> Unit = { android.util.Log.w("ChannelVaultHead", it) },
    delete: suspend (ByteArray) -> Unit,
): Int {
    var deleted = 0
    val finished = withTimeoutOrNull(timeoutMs) {
        for (id in channelIds) {
            try {
                delete(id)
                deleted += 1
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                warn("ADR-0064: a channel vault was not deleted: ${e.javaClass.simpleName}")
            }
        }
        true
    }
    if (finished == null) warn("ADR-0064: channel vault deletes stopped at the time bound")
    return deleted
}

/**
 * ADR-0064 section 1 — the owner deleted the channel: [deleteChannel] first
 * (its failure throws, and nothing else runs), then the channel's vault,
 * best effort: a vault delete that fails is logged and never undoes the
 * channel delete.
 */
public suspend fun deleteChannelThenVault(
    deleteChannel: suspend () -> Unit,
    warn: (String) -> Unit = { android.util.Log.w("ChannelVaultHead", it) },
    deleteVault: suspend () -> Unit,
) {
    deleteChannel()
    try {
        deleteVault()
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (e: Throwable) {
        warn("ADR-0064: the deleted channel's vault was not deleted: ${e.javaClass.simpleName}")
    }
}

/** The longest one head write may take before the commit goes on without
 *  it (chosen, not measured). */
public const val HEAD_WRITE_TIMEOUT_MS: Long = 20_000L

/**
 * The core and the storage service behind the head of an owned channel. Every
 * call is best effort for the caller that writes (a failed write never stops a
 * commit) and fail-open-to-waiting for the caller that reads.
 */
public object ChannelVaultHeads {
    private val client: VaultClient by lazy { VaultClient() }

    /** The vault ref of [channelId] (the ADR-0058 conversation ref). */
    public fun refOf(channelId: ByteArray): VaultRef =
        VaultRef.Conversation(FolderConversationRef(FolderConversationKind.CHANNEL, channelId))

    /** [ChannelIdentity.account] is the account's LINEAGE identity (the
     *  earliest own predecessor after an ADR-0074 rotation, else the current
     *  one): the seed the PublisherGroup id and the journal root derive from,
     *  so the vault stays the same across a rotation. */
    private suspend fun <T> withVault(ids: ChannelIdentity, channelId: ByteArray, block: suspend (VaultHeadStore, VaultHeadCodec) -> T): T {
        val keys = uniffi.lakko_ffi_uniffi.vaultKeys(ids.account)
        try {
            val ref = refOf(channelId)
            val idHex = me.lakko.app.storage.StorageClient.blobIdHex(keys.vaultId(ref))
            val writePk = keys.writePublicKey(ref)
            val signer = object : VaultRequestSigner {
                override val headerName: String = uniffi.lakko_ffi_uniffi.storageProofHeaderName()

                override fun sign(method: String, path: String, body: ByteArray): String = keys.signRequest(ref, method, path, body)
            }
            val store = object : VaultHeadStore {
                override suspend fun read() = client.readHead(idHex, signer)

                override suspend fun write(version: Long, sealed: ByteArray) = client.writeHead(idHex, version, sealed, signer)

                override suspend fun claim() = client.claim(idHex, writePk)
            }
            return block(store, NativeHeadCodec(keys, ref))
        } finally {
            keys.close()
        }
    }

    /** Whether this device's account owns [pg] (only the owner's devices
     *  keep the head; a co-publisher's device never writes). */
    private suspend fun ownsGroup(ids: ChannelIdentity, pg: PublisherGroup): Boolean =
        ids.mls { pg.ownerAccountKey() }.contentEquals(ids.accountKey())

    /** Before a staged commit of [pg] is sent: the head gets the epoch it
     *  moves to. `true` when written; never throws. */
    public suspend fun writeStaged(ids: ChannelIdentity, pg: PublisherGroup): Boolean =
        bestEffort("staged") {
            if (!ownsGroup(ids, pg)) return@bestEffort false
            val entry = ids.mls { device ->
                val info = pg.stagedGroupInfo(device) ?: return@mls null
                val seed = pg.stagedDsAccessSeed(device) ?: return@mls null
                ReentryEpoch(pg.epoch().toLong() + 1, seed, info)
            } ?: return@bestEffort false
            write(ids, pg, entry)
        }

    /** After [pg] applied a commit (another owner device's, or this one's
     *  entry merged): the head gets the current epoch. Never throws. */
    public suspend fun writeCurrent(ids: ChannelIdentity, pg: PublisherGroup, replace: Boolean = false): Boolean =
        bestEffort("current") {
            if (!ownsGroup(ids, pg)) return@bestEffort false
            val entry = ids.mls { device ->
                if (pg.hasStagedCommit() || !pg.isActive()) return@mls null
                ReentryEpoch(pg.epoch().toLong(), pg.dsAccessSeed(device), pg.groupInfo(device))
            } ?: return@bestEffort false
            write(ids, pg, entry, replace)
        }

    private suspend fun write(ids: ChannelIdentity, pg: PublisherGroup, entry: ReentryEpoch, replace: Boolean = false): Boolean {
        val channelId = pg.channelId()
        return try {
            withVault(ids, channelId) { store, codec -> writeVaultHead(store, codec, listOf(entry), replace) }
        } finally {
            entry.dsAccessSeed.fill(0)
        }
    }

    /** `DELETE /vault/{id}` of [channelId]'s vault under [accountIdentity]
     *  (the LINEAGE identity). Throws on failure; callers are best effort. */
    public suspend fun deleteVault(accountIdentity: uniffi.lakko_ffi_uniffi.Identity, channelId: ByteArray) {
        val keys = uniffi.lakko_ffi_uniffi.vaultKeys(accountIdentity)
        try {
            val ref = refOf(channelId)
            val idHex = me.lakko.app.storage.StorageClient.blobIdHex(keys.vaultId(ref))
            client.deleteVault(idHex, signerOf(keys, ref))
        } finally {
            keys.close()
        }
    }

    /**
     * Account deletion — every channel the account OWNS (the subscription
     * register, role Owner) has its vault deleted, best effort, bounded by
     * [VAULT_DELETE_TIMEOUT_MS]. [lineageSeed] is the account's lineage seed
     * (wiped here). A guest, or an account that owns none, sends nothing.
     */
    public suspend fun deleteOwnedVaults(lineageSeed: ByteArray) {
        val owned = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.channelSubscriptions?.value.orEmpty()
            .filter { it.role == uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.OWNER }
            .map { it.channelId }
        if (owned.isEmpty()) {
            lineageSeed.fill(0)
            return
        }
        val identity = try {
            me.lakko.app.auth.SessionSecretWipe.identity(lineageSeed)
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            lineageSeed.fill(0)
            return
        }
        identity.use { id -> deleteVaultsBounded(owned) { channelId -> deleteVault(id, channelId) } }
    }

    private fun signerOf(keys: VaultKeys, ref: VaultRef): VaultRequestSigner = object : VaultRequestSigner {
        override val headerName: String = uniffi.lakko_ffi_uniffi.storageProofHeaderName()

        override fun sign(method: String, path: String, body: ByteArray): String = keys.signRequest(ref, method, path, body)
    }

    /** The saved epochs of [channelId]'s head (this account's vault).
     *  Throws when the storage service could not be reached. */
    public suspend fun readReentry(ids: ChannelIdentity, channelId: ByteArray): List<ReentryEpoch> =
        withVault(ids, channelId) { store, codec -> readVaultReentry(store, codec) }

    private suspend fun bestEffort(what: String, block: suspend () -> Boolean): Boolean =
        try {
            withTimeoutOrNull(HEAD_WRITE_TIMEOUT_MS) { block() } ?: run {
                android.util.Log.w(TAG, "ADR-0064: the channel head ($what) was not written in time")
                false
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            android.util.Log.w(TAG, "ADR-0064: the channel head ($what) was not written: ${e.javaClass.simpleName}")
            false
        }

    private const val TAG: String = "ChannelVaultHead"
}

/** The core's head codec for one vault. */
private class NativeHeadCodec(private val keys: VaultKeys, private val ref: VaultRef) : VaultHeadCodec {
    override fun open(version: Long, sealed: ByteArray): OpenedHead {
        val plain = keys.openHead(ref, version.toULong(), sealed)
        try {
            val head = uniffi.lakko_ffi_uniffi.decodeConversationHead(plain)
            return OpenedHead(head.reentry.map { ReentryEpoch(it.epoch.toLong(), it.dsAccessSeed, it.groupInfo) })
        } finally {
            plain.fill(0)
        }
    }

    override fun seal(version: Long, reentry: List<ReentryEpoch>): ByteArray {
        val saved = reentry.map { VaultReentryEpoch(it.epoch.toULong(), it.dsAccessSeed, it.groupInfo) }
        val conversation = (ref as VaultRef.Conversation).conversation
        // Rebuilt on every write: only `reentry` is carried over from the
        // head read (the same on every platform; nothing else is read).
        val head = VaultConversationHead(
            conversation = conversation,
            kind = VaultConversationKind.UNKNOWN,
            title = "",
            people = emptyList(),
            membership = VaultMembership.IN,
            sharedPins = emptyList(),
            personalPins = emptyList(),
            lastActivityMs = System.currentTimeMillis().toULong(),
            chunks = emptyList(),
            tail = emptyList(),
            reentry = saved,
            registrarSeed = null,
        )
        val plain = uniffi.lakko_ffi_uniffi.encodeConversationHead(head)
        try {
            return keys.sealHead(ref, version.toULong(), plain)
        } finally {
            plain.fill(0)
        }
    }
}
