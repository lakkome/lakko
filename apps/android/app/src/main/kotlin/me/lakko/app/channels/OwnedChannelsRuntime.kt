// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Log
import me.lakko.app.auth.ExportKeyStore
import me.lakko.app.auth.NativeSealCore
import me.lakko.app.auth.SealSecret
import me.lakko.app.auth.SealStateStore
import me.lakko.app.storage.VaultClient
import me.lakko.app.storage.VaultRequestSigner
import uniffi.lakko_ffi_uniffi.ChannelRecoveryKey
import uniffi.lakko_ffi_uniffi.FfiOwnedAdmissionSeed
import uniffi.lakko_ffi_uniffi.FfiOwnedChannel
import uniffi.lakko_ffi_uniffi.FfiOwnedChannelsList
import uniffi.lakko_ffi_uniffi.FfiOwnedEpochSeed
import uniffi.lakko_ffi_uniffi.FfiOwnedPasskeyLock
import uniffi.lakko_ffi_uniffi.VaultRef

/*
 * ADR-0101 section 7 - the owned-channels list bound to the core, the
 * storage service and this device's channel state. The rules are in
 * `OwnedChannels.kt`; nothing here decides anything a test there does not
 * cover.
 */

/** [OwnedCore] over the native core (`owned_channels.rs`). */
public object NativeOwnedCore : OwnedCore {
    override fun passwordLockRoot(exportKey: ByteArray, serverHalf: ByteArray): ByteArray =
        uniffi.lakko_ffi_uniffi.ownedPasswordLockRoot(exportKey, serverHalf)

    override fun passkeyLockRoot(prfOutput: ByteArray, serverHalf: ByteArray): ByteArray =
        uniffi.lakko_ffi_uniffi.ownedPasskeyLockRoot(prfOutput, serverHalf)

    override fun newListKey(): ByteArray = uniffi.lakko_ffi_uniffi.generateOwnedChannelsListKey()

    override fun encodeListKeyLock(listKey: ByteArray): ByteArray =
        uniffi.lakko_ffi_uniffi.encodeOwnedListKeyLock(listKey)

    override fun decodeListKeyLock(head: ByteArray): ByteArray = uniffi.lakko_ffi_uniffi.decodeOwnedListKeyLock(head)

    override fun encodeKeysLock(listKey: ByteArray, serverHalf: ByteArray): ByteArray =
        uniffi.lakko_ffi_uniffi.encodeOwnedKeysLock(listKey, serverHalf)

    override fun decodeKeysLock(head: ByteArray): Pair<ByteArray, ByteArray> =
        uniffi.lakko_ffi_uniffi.decodeOwnedKeysLock(head).let { it.listKey to it.serverHalf }

    override fun encodeList(list: OwnedList): ByteArray = uniffi.lakko_ffi_uniffi.encodeOwnedChannelsList(ffi(list))

    override fun decodeList(bytes: ByteArray): OwnedList = view(uniffi.lakko_ffi_uniffi.decodeOwnedChannelsList(bytes))

    override fun merge(mine: OwnedList, theirs: OwnedList, deletedHex: List<String>): OwnedList = view(
        uniffi.lakko_ffi_uniffi.mergeOwnedChannelsLists(ffi(mine), ffi(theirs), deletedHex.map(::ownedUnhex)),
    )

    private fun ffi(list: OwnedList): FfiOwnedChannelsList = FfiOwnedChannelsList(
        passwordLockRoot = list.passwordLockRoot,
        passkeyLocks = list.passkeyLocks.map { (tag, root) -> FfiOwnedPasskeyLock(ownedUnhex(tag), root) },
        channels = list.channels.map { c ->
            FfiOwnedChannel(
                channelId = ownedUnhex(c.channelIdHex),
                readKey = c.readKey,
                handleSeed = c.handleSeed,
                admission = c.admission.map { FfiOwnedAdmissionSeed(it.version.toUInt(), it.seed) },
                epochs = c.epochs.map { FfiOwnedEpochSeed(it.epoch.toUInt(), it.seed) },
            )
        },
    )

    private fun view(list: FfiOwnedChannelsList): OwnedList = OwnedList(
        passwordLockRoot = list.passwordLockRoot,
        passkeyLocks = list.passkeyLocks.associate { ownedHex(it.tag) to it.root },
        channels = list.channels.map { c ->
            OwnedEntry(
                channelIdHex = ownedHex(c.channelId),
                readKey = c.readKey,
                handleSeed = c.handleSeed,
                admission = c.admission.map { OwnedAdmission(it.version.toInt(), it.seed) },
                epochs = c.epochs.map { OwnedEpoch(it.epochId.toLong(), it.seed) },
            )
        },
    )
}

/** [OwnedVaults] over the storage service (the ADR-0064 vault routes,
 *  unchanged): a vault's id, AEAD key and write key come from its root over
 *  the owned-channels reference. */
public class StorageOwnedVaults(private val client: VaultClient) : OwnedVaults {
    override fun of(root: ByteArray): OwnedVault = object : OwnedVault {
        private suspend fun <T> withKeys(block: suspend (uniffi.lakko_ffi_uniffi.VaultKeys, String, VaultRequestSigner) -> T): T {
            val keys = uniffi.lakko_ffi_uniffi.vaultKeysFromRoot(root)
            try {
                val idHex = me.lakko.app.storage.StorageClient.blobIdHex(keys.vaultId(REF))
                val signer = object : VaultRequestSigner {
                    override val headerName: String = uniffi.lakko_ffi_uniffi.storageProofHeaderName()

                    override fun sign(method: String, path: String, body: ByteArray): String =
                        keys.signRequest(REF, method, path, body)
                }
                return block(keys, idHex, signer)
            } finally {
                keys.close()
            }
        }

        override suspend fun read(): OwnedRead = withKeys { keys, idHex, signer ->
            when (val head = client.readHead(idHex, signer)) {
                is VaultClient.HeadRead.Found -> try {
                    OwnedRead.Found(head.version, keys.openHead(REF, head.version.toULong(), head.sealed))
                } catch (_: Throwable) {
                    OwnedRead.Unreadable
                }
                VaultClient.HeadRead.Missing, VaultClient.HeadRead.Refused -> OwnedRead.Missing
            }
        }

        override suspend fun write(version: Long, plain: ByteArray): OwnedWrite = withKeys { keys, idHex, signer ->
            val sealed = keys.sealHead(REF, version.toULong(), plain)
            when (client.writeHead(idHex, version, sealed, signer)) {
                VaultClient.HeadWrite.WRITTEN -> OwnedWrite.WRITTEN
                VaultClient.HeadWrite.CONFLICT -> OwnedWrite.CONFLICT
                VaultClient.HeadWrite.REFUSED -> {
                    // Nobody claimed it: claim (one `vault-claim` token),
                    // then write once more.
                    client.claim(idHex, keys.writePublicKey(REF))
                    when (client.writeHead(idHex, version, sealed, signer)) {
                        VaultClient.HeadWrite.WRITTEN -> OwnedWrite.WRITTEN
                        else -> OwnedWrite.CONFLICT
                    }
                }
            }
        }

        override suspend fun delete() {
            withKeys { _, idHex, signer -> client.deleteVault(idHex, signer) }
        }
    }

    private companion object {
        val REF: VaultRef = VaultRef.OwnedChannels
    }
}

/**
 * Before a key reset (ADR-0101 section 7 item 8): open the lock of what
 * sealed the reset and the list, and keep what was found for the channel
 * pass of the session that follows. Called from the reset, which holds the
 * seal secret and `S` at that moment; nothing here needs the account keys.
 */
public object OwnedChannelsRetake {
    private const val TAG = "OwnedChannels"

    /**
     * Open and keep. A reset sealed by the password opens the password lock,
     * one sealed by a passkey that passkey's lock. A reset sealed by the
     * phrase opens the KEYS lock (the phrase gives the old account seed; this
     * holds while the account never rotated its keys, since the keys lock is
     * named by the lineage seed). Throws on a network failure; finding no
     * lock keeps nothing.
     */
    public suspend fun prepare(context: Context, accountIdB64: String, sealedBy: SealSecret, serverHalf: ByteArray) {
        val core = NativeOwnedCore
        val root: ByteArray = when (sealedBy) {
            is SealSecret.Password -> core.passwordLockRoot(sealedBy.exportKey, serverHalf)
            is SealSecret.Passkey -> core.passkeyLockRoot(sealedBy.prfOutput, serverHalf)
            is SealSecret.Phrase -> uniffi.lakko_ffi_uniffi.RecoveryMnemonic.fromPhrase(sealedBy.phrase).use { m ->
                val seed = m.toSeed()
                try {
                    uniffi.lakko_ffi_uniffi.Identity.fromSeed(seed).use { uniffi.lakko_ffi_uniffi.ownedKeysLockRoot(it) }
                } finally {
                    seed.fill(0)
                }
            }
        }
        VaultClient().use { client ->
            val vaults = StorageOwnedVaults(client)
            val retake = if (sealedBy is SealSecret.Phrase) {
                openOwnedThroughKeysLock(root, core, vaults)
            } else {
                openOwnedForReset(root, serverHalf, core, vaults)
            }
            root.fill(0)
            if (retake == null) {
                Log.i(TAG, "ADR-0101: no owned-channels list was opened for the reset")
                return
            }
            try {
                OwnedChannelsSecretStore.onDevice(context).keepRetake(
                    account = accountIdB64,
                    listKey = retake.listKey,
                    serverHalf = retake.serverHalf,
                    encodedList = core.encodeList(retake.list),
                    owedHex = retake.list.channels.map { it.channelIdHex },
                )
                OwnedChannelsFactsStore.onDevice(context).clear(accountIdB64)
            } finally {
                retake.listKey.fill(0)
                retake.serverHalf.fill(0)
            }
        }
    }

    /** The reset did not happen: nothing of it is kept. */
    public fun discard(context: Context, accountIdB64: String) {
        runCatching { OwnedChannelsSecretStore.onDevice(context).clear(accountIdB64) }
    }
}

/** The owned-channels duties of a device that holds the account keys. */
public object OwnedChannelsRuntime {
    private const val TAG = "OwnedChannels"

    /** The accounts a full pass ran for in this process (a landing always
     *  runs one, whatever the digest says: another device may have written). */
    private val passed: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** Passkey seals made on this device now, for the pass that runs before
     *  the encrypted copy ([OwnedChannelsSecretStore.pendingPasskeys]) could
     *  be written: account to (tag to PRF output). */
    private val pendingPasskeys = java.util.concurrent.ConcurrentHashMap<String, Map<String, ByteArray>>()

    /** The application context the passkey seals are kept with; set by the
     *  session shell. */
    @Volatile
    public var appContext: Context? = null

    /** The channels this device deleted and the list still names. */
    private val deleted = java.util.concurrent.ConcurrentHashMap<String, Set<String>>()

    /** When this device last rotated a channel (for the 64-channel cap). */
    private val rotatedAt = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** Something the list hangs on changed (a channel created or deleted, a
     *  rotation, an admission seed, the password, a passkey): run a pass. */
    public fun notifyChanged() {
        ChannelReturnRuntime.notifyHeld()
    }

    /** A sign-out or an account change: the next landing runs a full pass. */
    public fun forget(accountIdB64: String) {
        passed.remove(accountIdB64)
        pendingPasskeys.remove(accountIdB64)?.values?.forEach { it.fill(0) }
        deleted.remove(accountIdB64)
    }

    /** `S` from the sign-up's answer: kept until the first pass put it under
     *  the keys lock. Never throws. */
    public fun keepServerHalf(context: Context, accountIdB64: String, serverHalf: ByteArray) {
        try {
            OwnedChannelsSecretStore.onDevice(context).keepServerHalf(accountIdB64, serverHalf)
        } catch (e: Throwable) {
            Log.w(TAG, "the owned-channels server half was not kept: ${e.javaClass.simpleName}")
        }
    }

    /** A passkey that keeps the chats was added on this device: its lock is
     *  made at the next pass. [prfOutput] is copied. */
    public fun passkeyAdded(accountIdB64: String, tagHex: String, prfOutput: ByteArray, context: Context? = appContext) {
        pendingPasskeys.merge(accountIdB64, mapOf(tagHex to prfOutput.copyOf())) { held, added -> held + added }
        // Kept under the device key until the lock is written, so a lock
        // owed when the process ends is still made (item C).
        context?.let { ctx ->
            try {
                OwnedChannelsSecretStore.onDevice(ctx).keepPendingPasskey(accountIdB64, tagHex, prfOutput)
            } catch (e: Throwable) {
                Log.w(TAG, "a passkey's owned-channels lock secret was not kept: ${e.javaClass.simpleName}")
            }
        }
        notifyChanged()
    }

    /** The owner deleted [channelIdHex] on this device. */
    public fun channelDeleted(accountIdB64: String, channelIdHex: String) {
        deleted.merge(accountIdB64, setOf(channelIdHex.lowercase())) { held, added -> held + added }
        notifyChanged()
    }

    /** This device rotated [channelIdHex] (or minted an admission seed, or
     *  created it): the list is written at the next pass. */
    public fun channelChanged(channelIdHex: String) {
        rotatedAt[channelIdHex.lowercase()] = System.currentTimeMillis()
        notifyChanged()
    }

    /** The channels this device owns: it publishes them and no co-publisher
     *  group id marks them. */
    internal fun ownedChannelIds(state: ChannelsUiState): List<String> =
        state.keysByChannel.value.keys.filter { state.publishesHere(it) && state.joinedGroupIdOf(it) == null }

    private suspend fun entryOf(context: Context, state: ChannelsUiState, account: String, idHex: String): OwnedEntry {
        val held = HashMap<UInt, ByteArray>(state.subscribedSeeds(idHex))
        for ((epoch, seed) in state.heldEpochSeeds(idHex)) held.putIfAbsent(epoch, seed)
        // Section 7 item 4: a channel with an @name carries the seed of the
        // handle key holding it, so a retake can move the name.
        val channel = state.channels.value.firstOrNull { it.id == idHex }
        val named = channel != null &&
            (channel.sharedMeta()?.fullRecord()?.handle != null || channel.handle.trim().removePrefix("@").isNotEmpty())
        val handleSeed = if (named) {
            runCatching { state.publisher?.withIdentity(persist = false) { ChannelDiscovery.handleWriteSeed(it, idHex) } }.getOrNull()
        } else {
            null
        }
        return ownedEntryOf(
            channelIdHex = idHex,
            readKey = ChannelReadKeys.get(context, account, idHex),
            admission = ChannelAdmissionKeys.all(context, account, idHex).map { OwnedAdmission(it.first.toInt(), it.second) },
            epochs = held.map { OwnedEpoch(it.key.toLong(), it.value) },
            handleSeed = handleSeed,
        )
    }

    /**
     * One pass for the signed-in account ([account], standard base64):
     * first what a reset still owes (take the listed channels back), then
     * the locks and the list, then the recovery keys of the owned channels.
     * Skipped, with no request, when nothing this device knows changed since
     * its last pass. Never throws but for cancellation.
     */
    @Suppress("LongMethod", "CyclomaticComplexMethod", "ReturnCount")
    public suspend fun pass(context: Context, state: ChannelsUiState, client: ChannelsClient, account: String) {
        val pub = state.publisher ?: return
        val secrets = OwnedChannelsSecretStore.onDevice(context)
        val factsStore = OwnedChannelsFactsStore.onDevice(context)
        var held = secrets.read(account)
        try {
            // --- after a recall: open the OLD keys lock the recall named
            // (section 7 item 8). A network failure leaves it owed and ends
            // the pass, so no new keys lock is made before the old R is in
            // hand.
            held.retakeRoot?.let { root ->
                val opened = VaultClient().use { openOwnedThroughKeysLock(root, NativeOwnedCore, StorageOwnedVaults(it)) }
                if (opened != null) {
                    try {
                        secrets.keepRetake(
                            account = account,
                            listKey = opened.listKey,
                            serverHalf = opened.serverHalf,
                            encodedList = NativeOwnedCore.encodeList(opened.list),
                            owedHex = opened.list.channels.map { it.channelIdHex },
                        )
                        factsStore.clear(account)
                    } finally {
                        opened.listKey.fill(0)
                        opened.serverHalf.fill(0)
                    }
                    Log.i(TAG, "ADR-0101: a recall opened the old owned-channels list")
                } else {
                    secrets.dropRetakeRoot(account)
                    Log.i(TAG, "ADR-0101: a recall found no owned-channels list")
                }
                held.serverHalf?.fill(0)
                held.listKey?.fill(0)
                held.retakeList?.fill(0)
                root.fill(0)
                held = secrets.read(account)
            }
            // --- after a reset: the channels the list named come back.
            if (held.owedHex.isNotEmpty() && held.listKey != null && held.retakeList != null) {
                val list = runCatching { NativeOwnedCore.decodeList(held.retakeList) }.getOrNull()
                val entries = list?.channels.orEmpty().filter { it.channelIdHex in held.owedHex }
                val refused = ArrayList<String>()
                val owed = retakeOwnedChannels(entries) { entry ->
                    val key = ChannelRecoveryKey.fromListKey(held.listKey, ownedUnhex(entry.channelIdHex))
                    val outcome = when (ChannelReturnRuntime.recoverOwnedChannel(context, state, client, account, entry, key)) {
                        RecoveryOutcome.OWNED -> OwnedRetakeOutcome.TAKEN
                        RecoveryOutcome.REFUSED -> OwnedRetakeOutcome.REFUSED
                        RecoveryOutcome.RETRY -> OwnedRetakeOutcome.RETRY
                    }
                    if (outcome == OwnedRetakeOutcome.REFUSED) refused += entry.channelIdHex
                    outcome
                }
                // A refused entry is dropped from the list too.
                if (refused.isNotEmpty()) deleted.merge(account, refused.toSet()) { a, b -> a + b }
                secrets.setOwed(account, owed.map { it.channelIdHex })
            }

            // --- the locks and the list.
            val view = SealStateStore.onDevice(context).view(account)
            val exportKey = runCatching { ExportKeyStore.getExportKey(context, account) }.getOrNull()
            // Only the account's CURRENT password's lock is made: this
            // device's export key counts when its seal key is the announced
            // one (a password changed elsewhere since is another's to lock).
            val currentExportKey = exportKey?.takeIf { key ->
                val pk = runCatching { NativeSealCore.entryOf(SealSecret.Password(key)).sealPkB64 }.getOrNull()
                pk != null && pk == view?.password?.sealPkB64
            }
            val owned = ownedChannelIds(state)
            val storedPasskeys = secrets.pendingPasskeys(account)
            val facts = OwnedDeviceFacts(
                keysLockRoot = pub.withIdentity(persist = false) { uniffi.lakko_ffi_uniffi.ownedKeysLockRoot(it) },
                currentExportKey = currentExportKey,
                heldServerHalf = held.serverHalf,
                heldListKey = held.listKey,
                channels = owned.map { entryOf(context, state, account, it) },
                rotatedAtMs = rotatedAt.toMap(),
                deletedHex = deleted[account].orEmpty().toList(),
                newPasskeys = storedPasskeys + pendingPasskeys[account].orEmpty(),
                // The passkeys that keep the chats are the ones the account
                // announced a seal for.
                livePasskeyTagsHex = view?.passkeys?.mapTo(HashSet()) { it.tagHex },
            )
            try {
                val digest = ownedFactsDigest(facts)
                if (account in passed && factsStore.digest(account) == digest) return
                val result = VaultClient().use { vaultClient ->
                    syncOwnedChannels(facts, NativeOwnedCore, StorageOwnedVaults(vaultClient))
                }
                when (result) {
                    is OwnedSync.Done -> {
                        try {
                            registerRecoveryKeys(context, state, client, account, owned, result.listKey)
                        } finally {
                            result.listKey.fill(0)
                        }
                        if (result.keysLockHolds) secrets.dropSecrets(account)
                        secrets.dropPendingPasskeys(account, facts.newPasskeys.keys)
                        pendingPasskeys.remove(account)?.values?.forEach { it.fill(0) }
                        deleted.remove(account)
                        passed += account
                        // The secrets just dropped change the digest: the one
                        // of the settled facts is what the next pass meets.
                        factsStore.setDigest(account, ownedFactsDigest(settled(facts)))
                        if (result.wrote) Log.i(TAG, "ADR-0101: the owned-channels list was written")
                    }
                    OwnedSync.NoServerHalf -> {
                        // An account made before the list existed: nothing to
                        // do here, and nothing to retry until a reset gives S.
                        passed += account
                        factsStore.setDigest(account, digest)
                    }
                    OwnedSync.NotNow -> Unit
                }
            } finally {
                facts.keysLockRoot.fill(0)
                exportKey?.fill(0)
                storedPasskeys.values.forEach { it.fill(0) }
            }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            Log.w(TAG, "the owned-channels pass did not finish: ${e.javaClass.simpleName}")
        } finally {
            held.serverHalf?.fill(0)
            held.listKey?.fill(0)
            held.retakeList?.fill(0)
            held.retakeRoot?.fill(0)
        }
    }

    /** [facts] once the keys lock holds what was held locally and the
     *  pending passkeys and deletions were applied. */
    private fun settled(facts: OwnedDeviceFacts): OwnedDeviceFacts = OwnedDeviceFacts(
        keysLockRoot = facts.keysLockRoot,
        currentExportKey = facts.currentExportKey,
        heldServerHalf = null,
        heldListKey = null,
        channels = facts.channels,
        rotatedAtMs = facts.rotatedAtMs,
        deletedHex = emptyList(),
        newPasskeys = emptyMap(),
        livePasskeyTagsHex = facts.livePasskeyTagsHex,
    )

    /**
     * Item 5: register, with the owner proof, the recovery key the list key
     * derives for every owned channel that has none recorded (a new channel,
     * and every channel made before this change, at its next pass). `R`
     * never changes, so neither a password change nor a rotation registers
     * anything again.
     */
    private suspend fun registerRecoveryKeys(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        account: String,
        owned: List<String>,
        listKey: ByteArray,
    ) {
        val store = RecoveryRegisteredStore.onDevice(context)
        val owing = channelsOwingRecoveryKey(owned, store.read(account))
        if (owing.isEmpty()) return
        val pub = state.publisher ?: return
        for (idHex in owing) {
            try {
                ChannelRecoveryKey.fromListKey(listKey, ownedUnhex(idHex)).use { key ->
                    pub.withIdentity(persist = false) { ChannelsClient.ownerSigner(it, idHex) }.use { owner ->
                        client.putRecoveryKey(idHex, owner, key.verifyingKey())
                    }
                }
                store.mark(account, idHex)
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (e: Throwable) {
                Log.i(TAG, "a channel recovery key was not registered yet: ${e.javaClass.simpleName}")
            }
        }
    }

    /**
     * Item 9: before `DELETE /account`, the list and every lock, bounded by
     * [timeoutMs]. Never throws but for cancellation.
     */
    public suspend fun deleteAll(context: Context, state: ChannelsUiState, account: String, timeoutMs: Long = VAULT_DELETE_TIMEOUT_MS) {
        val pub = state.publisher ?: return
        try {
            kotlinx.coroutines.withTimeoutOrNull(timeoutMs) {
                val root = pub.withIdentity(persist = false) { uniffi.lakko_ffi_uniffi.ownedKeysLockRoot(it) }
                try {
                    VaultClient().use { deleteOwnedChannels(root, NativeOwnedCore, StorageOwnedVaults(it)) }
                } finally {
                    root.fill(0)
                }
            } ?: Log.w(TAG, "ADR-0101: the owned-channels vault deletes stopped at the time bound")
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            Log.w(TAG, "ADR-0101: the owned-channels vaults were not deleted: ${e.javaClass.simpleName}")
        }
        runCatching { OwnedChannelsSecretStore.onDevice(context).clear(account) }
        runCatching { OwnedChannelsFactsStore.onDevice(context).clear(account) }
        forget(account)
    }

    /**
     * Item 9 from the account-deletion flow, which holds the lineage seed
     * and no channel state: the list and every lock, bounded by [timeoutMs].
     * [lineageSeed] is wiped. Never throws but for cancellation.
     */
    public suspend fun deleteAllWithSeed(
        context: Context,
        account: String,
        lineageSeed: ByteArray,
        timeoutMs: Long = VAULT_DELETE_TIMEOUT_MS,
    ) {
        try {
            kotlinx.coroutines.withTimeoutOrNull(timeoutMs) {
                val root = me.lakko.app.auth.SessionSecretWipe.identity(lineageSeed).use {
                    uniffi.lakko_ffi_uniffi.ownedKeysLockRoot(it)
                }
                try {
                    VaultClient().use { deleteOwnedChannels(root, NativeOwnedCore, StorageOwnedVaults(it)) }
                } finally {
                    root.fill(0)
                }
            } ?: Log.w(TAG, "ADR-0101: the owned-channels vault deletes stopped at the time bound")
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            Log.w(TAG, "ADR-0101: the owned-channels vaults were not deleted: ${e.javaClass.simpleName}")
        } finally {
            lineageSeed.fill(0)
        }
        runCatching { OwnedChannelsSecretStore.onDevice(context).clear(account) }
        runCatching { OwnedChannelsFactsStore.onDevice(context).clear(account) }
        forget(account)
    }
}
