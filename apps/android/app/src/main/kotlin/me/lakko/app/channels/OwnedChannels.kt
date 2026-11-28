// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.MarkerStorage

/*
 * ADR-0101 section 7 (rewritten 2026-11-26) - the list of the channels an
 * account owns, and its locks.
 *
 * The owner key of a channel is derived from the account seed, so a key
 * reset loses it and nobody else holds it. The account therefore keeps, on
 * the storage service, a LIST of the channels it owns, sealed under a random
 * list key `R`; each channel's recovery key is derived from `R`. `R` is kept
 * under locks:
 *
 *   - the PASSWORD lock, opened by the password's export key and a server
 *     half `S` that auth releases only at sign-up and after the password
 *     plus a second factor;
 *   - the KEYS lock, opened by the account's lineage seed (every device of
 *     the account holds it); its head also carries `S`;
 *   - a PASSKEY lock per passkey that keeps the chats, opened by that
 *     passkey's secret and `S`.
 *
 * Each is an ADR-0064 vault on the storage service, named by its root;
 * nothing names an account. A device with the keys opens the keys lock, then
 * the list. A device that reset the keys opens the lock of what sealed its
 * reset.
 *
 * This file is the rules and the orchestration, written against ports
 * ([OwnedCore], [OwnedVaults]) so the host suite, which has no native
 * library and no network, runs all of it. `OwnedChannelsRuntime.kt` binds
 * them to the core, the storage service and the channel state.
 */

/** One admission seed of a private channel. Secret: never logged. */
public class OwnedAdmission(public val version: Int, public val seed: ByteArray) {
    override fun toString(): String = "OwnedAdmission(version=$version)"
}

/** One epoch seed. Secret: never logged. */
public class OwnedEpoch(public val epoch: Long, public val seed: ByteArray) {
    override fun toString(): String = "OwnedEpoch(epoch=$epoch)"
}

/** One owned channel as the list carries it. */
public class OwnedEntry(
    /** 32 lower-case hex characters. */
    public val channelIdHex: String,
    /** The read key of a public channel, or `null`. */
    public val readKey: ByteArray?,
    public val admission: List<OwnedAdmission>,
    public val epochs: List<OwnedEpoch>,
    /** The channel's ADR-0087 handle write seed (`handleWriteSeed` of the
     *  journal root), or `null`: after a retake the new account signs the
     *  `rekey` of the channel's @name with it. Secret. */
    public val handleSeed: ByteArray? = null,
) {
    override fun toString(): String =
        "OwnedEntry(admission=${admission.size}, epochs=${epochs.size}, handleSeed=${handleSeed != null})"
}

/** The list. Roots are secret (a root opens its lock). */
public class OwnedList(
    /** The current password lock's 32-byte root; all zero when none was
     *  made yet (a device that holds no export key made the list). */
    public val passwordLockRoot: ByteArray,
    /** Passkey tag (32 hex characters) to the lock's 32-byte root. */
    public val passkeyLocks: Map<String, ByteArray>,
    public val channels: List<OwnedEntry>,
) {
    override fun toString(): String = "OwnedList(channels=${channels.size}, passkeyLocks=${passkeyLocks.size})"
}

/** The core's bounds (`lakko_channels::owned`). */
public const val OWNED_MAX_CHANNELS: Int = 64
public const val OWNED_MAX_ADMISSION: Int = 8
public const val OWNED_MAX_EPOCHS: Int = 4
public const val OWNED_MAX_PASSKEY_LOCKS: Int = 8

/** A root nobody made a lock for. */
public val OWNED_NO_ROOT: ByteArray get() = ByteArray(32)

private fun ByteArray.isNoRoot(): Boolean = all { it == 0.toByte() }

/**
 * One channel's entry from what this device holds: the read key, the newest
 * [OWNED_MAX_ADMISSION] admission seeds (by version, then by seed bytes, so
 * two devices keep the same ones) and the newest [OWNED_MAX_EPOCHS] epoch
 * seeds. Four epoch seeds is the ADR-0031 retention: the owner reads what a
 * returning subscriber reads.
 */
public fun ownedEntryOf(
    channelIdHex: String,
    readKey: ByteArray?,
    admission: List<OwnedAdmission>,
    epochs: List<OwnedEpoch>,
    handleSeed: ByteArray? = null,
): OwnedEntry = OwnedEntry(
    channelIdHex = channelIdHex.lowercase(),
    readKey = readKey?.takeIf { it.size == 32 && !it.isNoRoot() },
    handleSeed = handleSeed?.takeIf { it.size == 32 && !it.isNoRoot() },
    admission = admission.filter { it.seed.size == 32 && it.version >= 0 }
        .distinctBy { it.version to ownedHex(it.seed) }
        .sortedWith(compareBy<OwnedAdmission> { it.version }.thenBy { ownedHex(it.seed) })
        .takeLast(OWNED_MAX_ADMISSION),
    epochs = epochs.filter { it.seed.size == 32 && it.epoch >= 0 }
        .associateBy { it.epoch }.values.sortedBy { it.epoch }
        .takeLast(OWNED_MAX_EPOCHS),
)

/**
 * The channels the list should hold, at most [OWNED_MAX_CHANNELS]: when the
 * account owns more, the ones it rotated most recently ([rotatedAtMs], by
 * channel id; a channel with no time sorts as never), ties by channel id.
 */
public fun ownedEntriesToList(entries: List<OwnedEntry>, rotatedAtMs: Map<String, Long> = emptyMap()): List<OwnedEntry> =
    entries.distinctBy { it.channelIdHex }
        .sortedWith(compareByDescending<OwnedEntry> { rotatedAtMs[it.channelIdHex] ?: 0L }.thenBy { it.channelIdHex })
        .take(OWNED_MAX_CHANNELS)
        .sortedBy { it.channelIdHex }

/** A stable digest of a list's content, for "did anything change": the
 *  SHA-256 of a canonical rendering. Reveals nothing by itself and is kept
 *  only on this device. */
public fun ownedListDigest(list: OwnedList): String {
    val md = java.security.MessageDigest.getInstance("SHA-256")
    md.update(list.passwordLockRoot)
    for ((tag, root) in list.passkeyLocks.toSortedMap()) {
        md.update(tag.toByteArray(Charsets.UTF_8))
        md.update(root)
    }
    for (c in list.channels.sortedBy { it.channelIdHex }) {
        md.update(c.channelIdHex.toByteArray(Charsets.UTF_8))
        md.update(byteArrayOf(if (c.readKey != null) 1 else 0))
        c.readKey?.let(md::update)
        md.update(byteArrayOf(if (c.handleSeed != null) 1 else 0))
        c.handleSeed?.let(md::update)
        for (a in c.admission) {
            md.update(a.version.toString().toByteArray(Charsets.UTF_8))
            md.update(a.seed)
        }
        md.update(0x7c)
        for (e in c.epochs) {
            md.update(e.epoch.toString().toByteArray(Charsets.UTF_8))
            md.update(e.seed)
        }
        md.update(0x0a)
    }
    return ownedHex(md.digest())
}

internal fun ownedHex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

internal fun ownedUnhex(text: String): ByteArray =
    ByteArray(text.length / 2) { text.substring(it * 2, it * 2 + 2).toInt(16).toByte() }

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/** The core's owned-channels functions (`owned_channels.rs`). */
public interface OwnedCore {
    public fun passwordLockRoot(exportKey: ByteArray, serverHalf: ByteArray): ByteArray

    public fun passkeyLockRoot(prfOutput: ByteArray, serverHalf: ByteArray): ByteArray

    /** A fresh random list key `R`. */
    public fun newListKey(): ByteArray

    /** The head of a password or passkey lock (`R`). */
    public fun encodeListKeyLock(listKey: ByteArray): ByteArray

    /** `R` of a password or passkey lock head; throws when malformed. */
    public fun decodeListKeyLock(head: ByteArray): ByteArray

    /** The head of the keys lock (`R || S`). */
    public fun encodeKeysLock(listKey: ByteArray, serverHalf: ByteArray): ByteArray

    /** `(R, S)` of a keys lock head; throws when malformed. */
    public fun decodeKeysLock(head: ByteArray): Pair<ByteArray, ByteArray>

    public fun encodeList(list: OwnedList): ByteArray

    /** Throws when the bytes are not a list. */
    public fun decodeList(bytes: ByteArray): OwnedList

    /** The merge after a version conflict: [mine] is what this device wanted
     *  to write, [theirs] what it read back, [deletedHex] the channels this
     *  device deleted. */
    public fun merge(mine: OwnedList, theirs: OwnedList, deletedHex: List<String>): OwnedList
}

/** What a vault read found. */
public sealed interface OwnedRead {
    /** The head's plaintext at [version]. */
    public class Found(public val version: Long, public val plain: ByteArray) : OwnedRead

    /** Nothing there: never claimed, or claimed and empty. */
    public data object Missing : OwnedRead

    /** A head is there and this root does not open it. */
    public data object Unreadable : OwnedRead
}

/** What a vault write did. */
public enum class OwnedWrite { WRITTEN, CONFLICT }

/** One owned-channels vault, named by its root. Network failures throw. */
public interface OwnedVault {
    public suspend fun read(): OwnedRead

    /** Write [plain] at [version] (claiming the vault first when nobody
     *  has). [OwnedWrite.CONFLICT] when another write took that version. */
    public suspend fun write(version: Long, plain: ByteArray): OwnedWrite

    /** Delete the vault. Nothing there is not an error. */
    public suspend fun delete()
}

/** The vaults, by root. */
public fun interface OwnedVaults {
    public fun of(root: ByteArray): OwnedVault
}

// ---------------------------------------------------------------------------
// A device with the keys
// ---------------------------------------------------------------------------

/** What this device holds for one pass. Secrets; the caller wipes them. */
public class OwnedDeviceFacts(
    /** The keys lock's root (from the lineage seed). */
    public val keysLockRoot: ByteArray,
    /**
     * The export key of the account's CURRENT password, or `null` when this
     * device holds none or the one it holds is not the announced one (a
     * password changed elsewhere since): only the current password's lock is
     * ever made.
     */
    public val currentExportKey: ByteArray?,
    /** `S` held locally (from sign-up, or fetched for a reset) while no keys
     *  lock carries it yet; `null` otherwise. */
    public val heldServerHalf: ByteArray?,
    /** `R` taken from the lock a reset opened, to be put under the NEW keys
     *  lock; `null` otherwise (a fresh one is made when none exists). */
    public val heldListKey: ByteArray?,
    /** The channels this device knows the account owns. */
    public val channels: List<OwnedEntry>,
    /** Channel id to when this device last rotated it (for the cap). */
    public val rotatedAtMs: Map<String, Long> = emptyMap(),
    /** The channels this device deleted (they leave the list). */
    public val deletedHex: List<String> = emptyList(),
    /** Passkey seals made on this device now: tag to PRF output. */
    public val newPasskeys: Map<String, ByteArray> = emptyMap(),
    /** The tags of the account's passkeys that keep the chats now; `null`
     *  when not known (no passkey lock is deleted then). */
    public val livePasskeyTagsHex: Set<String>? = null,
)

/** How a pass ended. */
public sealed interface OwnedSync {
    /** The locks and the list are as they should be. [listKey] is `R`, for
     *  the recovery keys; the caller wipes it. */
    public class Done(
        public val listKey: ByteArray,
        public val list: OwnedList,
        /** Something was written. */
        public val wrote: Boolean,
        /** The keys lock now carries `S` and `R`: what was held locally for
         *  it can be dropped. */
        public val keysLockHolds: Boolean,
    ) : OwnedSync

    /** No keys lock exists and this device holds no `S` to make one with
     *  (an account made before the list existed, or a device that never saw
     *  the sign-up): nothing is made. */
    public data object NoServerHalf : OwnedSync

    /** The keys lock exists and this device's lineage seed does not open it,
     *  or a write kept losing: left for the next pass. */
    public data object NotNow : OwnedSync
}

/** How many read-merge-write rounds one list write takes at most. Chosen. */
public const val OWNED_WRITE_ATTEMPTS: Int = 3

/**
 * One pass of a device that holds the account keys (items 4, 6 and 7):
 *
 *   1. the keys lock: read it; make it (`R || S`) when nobody has, with the
 *      held `S` and the held `R` (a reset) or a fresh one;
 *   2. the list: read it, merge what this device knows into it (by channel
 *      id; a deleted channel dropped; newer epochs and admission versions
 *      kept), set the lock roots, and write it when it changed. A `409`
 *      reads, merges and writes again;
 *   3. the password lock: when this device holds the CURRENT password's
 *      export key and the list names another root (or none), make the lock
 *      at the new root, record it, and delete the old one;
 *   4. the passkey locks: one for each passkey just added here, the locks of
 *      passkeys that no longer keep the chats deleted.
 *
 * The lock a root names is written BEFORE the list records that root, and
 * the old lock is deleted only after the list no longer names it, so the
 * list never points at a lock that does not exist.
 */
@Suppress("CyclomaticComplexMethod", "LongMethod", "ReturnCount")
public suspend fun syncOwnedChannels(facts: OwnedDeviceFacts, core: OwnedCore, vaults: OwnedVaults): OwnedSync {
    // 1. The keys lock.
    val keysVault = vaults.of(facts.keysLockRoot)
    var listKey: ByteArray
    val serverHalf: ByteArray
    var wrote = false
    when (val read = keysVault.read()) {
        is OwnedRead.Found -> {
            val (r, s) = try {
                core.decodeKeysLock(read.plain)
            } catch (_: Throwable) {
                return OwnedSync.NotNow
            }
            listKey = r
            serverHalf = s
        }
        OwnedRead.Unreadable -> return OwnedSync.NotNow
        OwnedRead.Missing -> {
            val s = facts.heldServerHalf ?: return OwnedSync.NoServerHalf
            listKey = facts.heldListKey?.copyOf() ?: core.newListKey()
            serverHalf = s
            if (keysVault.write(1L, core.encodeKeysLock(listKey, serverHalf)) != OwnedWrite.WRITTEN) {
                // Another device of the account made it at the same moment:
                // its R is the account's, read at the next pass.
                return OwnedSync.NotNow
            }
            wrote = true
        }
    }

    // 2-4. The list, with the lock roots it should name.
    val desiredChannels = ownedEntriesToList(facts.channels, facts.rotatedAtMs)
    val wantedPasswordRoot = facts.currentExportKey?.let { core.passwordLockRoot(it, serverHalf) }
    val newPasskeyRoots = facts.newPasskeys.mapValues { (_, prf) -> core.passkeyLockRoot(prf, serverHalf) }
    val listVault = vaults.of(listKey)
    repeat(OWNED_WRITE_ATTEMPTS) {
        val read = listVault.read()
        val (version, stored) = when (read) {
            is OwnedRead.Found -> read.version to try {
                core.decodeList(read.plain)
            } catch (_: Throwable) {
                null
            }
            // A head this key cannot open is written over: it is ours.
            OwnedRead.Unreadable -> 0L to null
            OwnedRead.Missing -> 0L to null
        }
        val passwordRoot = wantedPasswordRoot ?: stored?.passwordLockRoot ?: OWNED_NO_ROOT
        val keptPasskeys = stored?.passkeyLocks.orEmpty().filter { (tag, _) ->
            tag !in newPasskeyRoots && (facts.livePasskeyTagsHex == null || tag in facts.livePasskeyTagsHex)
        }
        val passkeyLocks = (keptPasskeys + newPasskeyRoots).entries
            .sortedBy { it.key }.take(OWNED_MAX_PASSKEY_LOCKS).associate { it.key to it.value }
        val mine = OwnedList(passwordRoot, passkeyLocks, desiredChannels)
        // What the list holds for channels this device does not know
        // (another owner device's) is kept by the merge.
        val next = if (stored == null) mine else core.merge(mine, stored, facts.deletedHex)
        // The roots are this pass's decision, whatever the merge kept.
        val target = OwnedList(passwordRoot, passkeyLocks, next.channels)
        val changed = stored == null || ownedListDigest(stored) != ownedListDigest(target)
        if (!changed) {
            return OwnedSync.Done(listKey, target, wrote, keysLockHolds = true)
        }
        // A lock is made before the list names it.
        if (wantedPasswordRoot != null &&
            (stored == null || !stored.passwordLockRoot.contentEquals(wantedPasswordRoot))
        ) {
            writeLock(vaults.of(wantedPasswordRoot), core.encodeListKeyLock(listKey))
        }
        for ((tag, root) in newPasskeyRoots) {
            if (stored?.passkeyLocks?.get(tag)?.contentEquals(root) != true) {
                writeLock(vaults.of(root), core.encodeListKeyLock(listKey))
            }
        }
        if (listVault.write(version + 1, core.encodeList(target)) == OwnedWrite.WRITTEN) {
            // Only now are the locks the list no longer names deleted.
            if (stored != null) {
                if (!stored.passwordLockRoot.isNoRoot() && !stored.passwordLockRoot.contentEquals(passwordRoot)) {
                    deleteQuietly(vaults.of(stored.passwordLockRoot))
                }
                for ((tag, root) in stored.passkeyLocks) {
                    if (passkeyLocks[tag]?.contentEquals(root) != true) deleteQuietly(vaults.of(root))
                }
            }
            return OwnedSync.Done(listKey, target, wrote = true, keysLockHolds = true)
        }
        // 409: another owner device wrote; read, merge and write again.
    }
    return OwnedSync.NotNow
}

/** Write a lock head: at version 1 when the vault is new, else over what is
 *  there (a lock is only ever `R`). A lost race reads and writes again;
 *  a lock still not written throws, so the pass is owed again and the list
 *  never names a lock that does not exist (section 7, alignment
 *  2026-11-27: a lock write is retried until it succeeds, never dropped). */
private suspend fun writeLock(vault: OwnedVault, head: ByteArray) {
    repeat(OWNED_WRITE_ATTEMPTS) {
        val version = when (val read = vault.read()) {
            is OwnedRead.Found -> if (read.plain.contentEquals(head)) return else read.version
            OwnedRead.Missing, OwnedRead.Unreadable -> 0L
        }
        if (vault.write(version + 1, head) == OwnedWrite.WRITTEN) return
    }
    throw OwnedLockNotWritten()
}

/** A lock could not be written in this pass: owed to the next one. */
public class OwnedLockNotWritten : Exception("an owned-channels lock was not written")

private suspend fun deleteQuietly(vault: OwnedVault) {
    try {
        vault.delete()
    } catch (cancel: kotlinx.coroutines.CancellationException) {
        throw cancel
    } catch (_: Throwable) {
        // A stale lock left behind opens the same R; it is deleted with the
        // account, and whoever holds R may delete it at a later pass.
    }
}

// ---------------------------------------------------------------------------
// After a reset (item 8)
// ---------------------------------------------------------------------------

/** What opened the list for a reset: `R`, `S` and the list as it stood. */
public class OwnedRetake(
    public val listKey: ByteArray,
    public val serverHalf: ByteArray,
    public val list: OwnedList,
) {
    override fun toString(): String = "OwnedRetake(channels=${list.channels.size})"
}

/**
 * Before the reset: open the lock at [lockRoot] (the password lock, or the
 * passkey lock of the passkey that sealed the reset), then the list. `null`
 * when the lock or the list is not there or does not open: no channel comes
 * back, and the reset goes on. Network failures throw.
 */
public suspend fun openOwnedForReset(
    lockRoot: ByteArray,
    serverHalf: ByteArray,
    core: OwnedCore,
    vaults: OwnedVaults,
): OwnedRetake? {
    val lock = vaults.of(lockRoot).read() as? OwnedRead.Found ?: return null
    val listKey = try {
        core.decodeListKeyLock(lock.plain)
    } catch (_: Throwable) {
        return null
    }
    val stored = vaults.of(listKey).read() as? OwnedRead.Found ?: return null
    val list = try {
        core.decodeList(stored.plain)
    } catch (_: Throwable) {
        return null
    }
    return OwnedRetake(listKey, serverHalf.copyOf(), list)
}

/**
 * Open the account's list through a KEYS lock at [root] (whose head is
 * `R || S`): a reset sealed by the phrase, or a recall, which holds the old
 * lineage seed (section 7 item 8). `null` when the lock or the list is not
 * there or does not open. Network failures throw.
 */
public suspend fun openOwnedThroughKeysLock(root: ByteArray, core: OwnedCore, vaults: OwnedVaults): OwnedRetake? {
    val lock = vaults.of(root).read() as? OwnedRead.Found ?: return null
    val (r, s) = try {
        core.decodeKeysLock(lock.plain)
    } catch (_: Throwable) {
        return null
    }
    val stored = vaults.of(r).read() as? OwnedRead.Found ?: return null
    val list = try {
        core.decodeList(stored.plain)
    } catch (_: Throwable) {
        return null
    }
    return OwnedRetake(r, s, list)
}

/** What taking one channel back came to. */
public enum class OwnedRetakeOutcome {
    /** The owner key is this account's again. */
    TAKEN,

    /** The channels service refused the recovery proof (`401`, or the
     *  channel is gone): the entry is dropped. */
    REFUSED,

    /** No answer: tried again at the next pass. */
    RETRY,
}

/**
 * After the reset: take every listed channel back ([take]). Returns the
 * entries still owed (no answer yet); the taken and the refused ones are
 * done. Never throws but for cancellation.
 */
public suspend fun retakeOwnedChannels(
    entries: List<OwnedEntry>,
    take: suspend (OwnedEntry) -> OwnedRetakeOutcome,
): List<OwnedEntry> {
    val owed = ArrayList<OwnedEntry>()
    for (entry in entries) {
        val outcome = try {
            take(entry)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            OwnedRetakeOutcome.RETRY
        }
        if (outcome == OwnedRetakeOutcome.RETRY) owed += entry
    }
    return owed
}

// ---------------------------------------------------------------------------
// Account deletion (item 9)
// ---------------------------------------------------------------------------

/**
 * Before `DELETE /account`: delete the list and every lock it names, then
 * the keys lock. Best effort and bounded by the caller: a vault that is not
 * deleted holds only ciphertext nothing else names.
 */
public suspend fun deleteOwnedChannels(keysLockRoot: ByteArray, core: OwnedCore, vaults: OwnedVaults) {
    val keysVault = vaults.of(keysLockRoot)
    val keys = keysVault.read() as? OwnedRead.Found
    if (keys != null) {
        val listKey = try {
            core.decodeKeysLock(keys.plain).first
        } catch (_: Throwable) {
            null
        }
        if (listKey != null) {
            val listVault = vaults.of(listKey)
            val stored = (listVault.read() as? OwnedRead.Found)?.let {
                try {
                    core.decodeList(it.plain)
                } catch (_: Throwable) {
                    null
                }
            }
            if (stored != null) {
                if (!stored.passwordLockRoot.isNoRoot()) deleteQuietly(vaults.of(stored.passwordLockRoot))
                for (root in stored.passkeyLocks.values) deleteQuietly(vaults.of(root))
            }
            deleteQuietly(listVault)
        }
    }
    deleteQuietly(keysVault)
}

// ---------------------------------------------------------------------------
// What this device keeps
// ---------------------------------------------------------------------------

/**
 * Secrets this device holds only until a keys lock carries them, in the
 * ENCRYPTED file: `S` from the sign-up (until the first pass made the keys
 * lock; a guest keeps it until its claim), and after a reset `R`, `S` and
 * the list entries still to take back. Removed as soon as the storage
 * service holds them under a lock again.
 */
public class OwnedChannelsSecretStore(private val storage: MarkerStorage) {

    @Serializable
    private class Row(
        val serverHalfB64: String? = null,
        val listKeyB64: String? = null,
        /** The encoded list a reset opened (standard base64), while entries
         *  are still owed. */
        val retakeListB64: String? = null,
        /** The channel ids still to take back. */
        val owedHex: List<String> = emptyList(),
        /** A recall's OLD keys lock root (ADR-0101 section 7 item 8), still
         *  to be opened by the channel pass. */
        val retakeRootB64: String? = null,
    )

    /** What is held for [account]. The caller wipes the arrays. */
    public class Held(
        public val serverHalf: ByteArray?,
        public val listKey: ByteArray?,
        public val retakeList: ByteArray?,
        public val owedHex: List<String>,
        /** A recall's old keys lock root still to open, or `null`. */
        public val retakeRoot: ByteArray? = null,
    )

    private fun row(account: String): Row? =
        runCatching { storage.get(KEY_PREFIX + account) }.getOrNull()
            ?.let { raw -> runCatching { json.decodeFromString(Row.serializer(), raw) }.getOrNull() }

    public fun read(account: String): Held {
        val row = row(account) ?: return Held(null, null, null, emptyList())
        fun bytes(b64: String?, size: Int?): ByteArray? = b64?.let {
            runCatching { java.util.Base64.getDecoder().decode(it) }.getOrNull()
        }?.takeIf { size == null || it.size == size }
        return Held(
            serverHalf = bytes(row.serverHalfB64, 32),
            listKey = bytes(row.listKeyB64, 32),
            retakeList = bytes(row.retakeListB64, null),
            owedHex = row.owedHex,
            retakeRoot = bytes(row.retakeRootB64, 32),
        )
    }

    /** A recall took the account back: the old keys lock at [root] is to be
     *  opened by the next channel pass. Throws when storage refuses. */
    public fun keepRetakeRoot(account: String, root: ByteArray) {
        val held = row(account)
        save(
            account,
            Row(held?.serverHalfB64, held?.listKeyB64, held?.retakeListB64, held?.owedHex.orEmpty(), enc(root)),
        )
    }

    /** The old keys lock was opened (or holds nothing): forget its root. */
    public fun dropRetakeRoot(account: String) {
        val held = row(account) ?: return
        if (held.retakeRootB64 == null) return
        val next = Row(held.serverHalfB64, held.listKeyB64, held.retakeListB64, held.owedHex, null)
        if (next.serverHalfB64 == null && next.listKeyB64 == null && next.retakeListB64 == null && next.owedHex.isEmpty()) {
            runCatching { storage.remove(KEY_PREFIX + account) }
        } else {
            save(account, next)
        }
    }

    /** Keep `S` (the sign-up's answer). Throws when storage refuses. */
    public fun keepServerHalf(account: String, serverHalf: ByteArray) {
        val held = row(account)
        save(account, Row(enc(serverHalf), held?.listKeyB64, held?.retakeListB64, held?.owedHex.orEmpty(), held?.retakeRootB64))
    }

    /** Keep what a reset opened. Throws when storage refuses. */
    public fun keepRetake(account: String, listKey: ByteArray, serverHalf: ByteArray, encodedList: ByteArray, owedHex: List<String>) {
        save(account, Row(enc(serverHalf), enc(listKey), enc(encodedList), owedHex))
    }

    /** The entries still owed after a pass. */
    public fun setOwed(account: String, owedHex: List<String>) {
        val held = row(account) ?: return
        save(
            account,
            Row(held.serverHalfB64, held.listKeyB64, if (owedHex.isEmpty()) null else held.retakeListB64, owedHex, held.retakeRootB64),
        )
    }

    /** The keys lock holds `R` and `S` now: nothing is kept here, unless
     *  entries are still owed to a retake. */
    public fun dropSecrets(account: String) {
        val held = row(account) ?: return
        if (held.owedHex.isNotEmpty() || held.retakeRootB64 != null) return
        runCatching { storage.remove(KEY_PREFIX + account) }
    }

    /** Everything (a refused reset, Forget device, a wipe). */
    public fun clear(account: String) {
        runCatching { storage.remove(KEY_PREFIX + account) }
        runCatching { storage.remove(PASSKEYS_PREFIX + account) }
    }

    /**
     * The passkey seals made on this device whose lock is not written yet:
     * tag (32 hex) to PRF output. Kept here so a lock owed when the process
     * ends is still made at the next pass (alignment 2026-11-27, item C).
     * The caller wipes the arrays.
     */
    public fun pendingPasskeys(account: String): Map<String, ByteArray> {
        val raw = runCatching { storage.get(PASSKEYS_PREFIX + account) }.getOrNull() ?: return emptyMap()
        val map = runCatching {
            json.decodeFromString(passkeysSerializer, raw)
        }.getOrNull() ?: return emptyMap()
        return map.mapNotNull { (tag, b64) ->
            runCatching { java.util.Base64.getDecoder().decode(b64) }.getOrNull()
                ?.takeIf { it.size == 32 }?.let { tag to it }
        }.toMap()
    }

    /** Keep [prfOutput] for [tagHex] until its lock is written. Throws when
     *  storage refuses. */
    public fun keepPendingPasskey(account: String, tagHex: String, prfOutput: ByteArray) {
        val held = pendingPasskeys(account)
        val next = held.mapValues { enc(it.value) } + (tagHex to enc(prfOutput))
        held.values.forEach { it.fill(0) }
        me.lakko.app.account.AccountStoreGate.writeOrEnd(account) {
            storage.set(
                PASSKEYS_PREFIX + account,
                json.encodeToString(passkeysSerializer, next),
            )
        }
    }

    /** The locks of [tagsHex] are written: forget their secrets. */
    public fun dropPendingPasskeys(account: String, tagsHex: Set<String>) {
        if (tagsHex.isEmpty()) return
        val held = pendingPasskeys(account)
        val next = held.filterKeys { it !in tagsHex }.mapValues { enc(it.value) }
        held.values.forEach { it.fill(0) }
        runCatching {
            if (next.isEmpty()) {
                storage.remove(PASSKEYS_PREFIX + account)
            } else {
                storage.set(
                    PASSKEYS_PREFIX + account,
                    json.encodeToString(passkeysSerializer, next),
                )
            }
        }
    }

    private fun save(account: String, row: Row) {
        me.lakko.app.account.AccountStoreGate.writeOrEnd(account) {
            storage.set(KEY_PREFIX + account, json.encodeToString(Row.serializer(), row))
        }
    }

    private fun enc(bytes: ByteArray): String = java.util.Base64.getEncoder().encodeToString(bytes)

    public companion object {
        public const val KEY_PREFIX: String = "owned-channels-secret:"
        public const val PASSKEYS_PREFIX: String = "owned-channels-passkeys:"
        private val passkeysSerializer = kotlinx.serialization.builtins.MapSerializer(
            kotlinx.serialization.serializer<String>(),
            kotlinx.serialization.serializer<String>(),
        )
        private val json = Json { ignoreUnknownKeys = true }

        public fun onDevice(context: android.content.Context): OwnedChannelsSecretStore =
            OwnedChannelsSecretStore(me.lakko.app.auth.ResetReturnStorage.secret(context))
    }
}

/**
 * What this device last wrote, in the plain file: the digest of the facts a
 * pass was made from, so a pass with nothing new makes no request. No
 * secret: a SHA-256 of secrets and public ids together.
 */
public class OwnedChannelsFactsStore(private val storage: MarkerStorage) {
    public fun digest(account: String): String? = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull()

    public fun setDigest(account: String, digest: String) {
        try {
            me.lakko.app.account.AccountStoreGate.write(account) { storage.set(KEY_PREFIX + account, digest) }
        } catch (_: Throwable) {
            // The next pass reads the vaults again: no harm.
        }
    }

    public fun clear(account: String) {
        runCatching { storage.remove(KEY_PREFIX + account) }
    }

    public companion object {
        public const val KEY_PREFIX: String = "owned-channels-facts:"

        public fun onDevice(context: android.content.Context): OwnedChannelsFactsStore =
            OwnedChannelsFactsStore(me.lakko.app.auth.ResetReturnStorage.plain(context))
    }
}

/**
 * The digest of what a pass would be made from: the list this device wants,
 * whether it holds the current password's key, its pending secrets and the
 * passkey tags. Equal digests mean a pass would change nothing this device
 * knows of (another device's writes are picked up at the next landing, which
 * always runs a pass).
 */
public fun ownedFactsDigest(facts: OwnedDeviceFacts): String {
    val md = java.security.MessageDigest.getInstance("SHA-256")
    md.update(facts.keysLockRoot)
    md.update(byteArrayOf(if (facts.currentExportKey != null) 1 else 0))
    facts.currentExportKey?.let(md::update)
    md.update(byteArrayOf(if (facts.heldServerHalf != null) 1 else 0, if (facts.heldListKey != null) 1 else 0))
    md.update(
        ownedListDigest(OwnedList(OWNED_NO_ROOT, emptyMap(), ownedEntriesToList(facts.channels, facts.rotatedAtMs)))
            .toByteArray(Charsets.UTF_8),
    )
    for (id in facts.deletedHex.sorted()) md.update(id.toByteArray(Charsets.UTF_8))
    for (tag in facts.newPasskeys.keys.sorted()) md.update(tag.toByteArray(Charsets.UTF_8))
    md.update((facts.livePasskeyTagsHex?.sorted()?.joinToString(",") ?: "?").toByteArray(Charsets.UTF_8))
    return ownedHex(md.digest())
}
