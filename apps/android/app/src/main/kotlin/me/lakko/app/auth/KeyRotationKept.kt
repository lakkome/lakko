// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import java.io.File
import java.nio.ByteBuffer

/*
 * ADR-0066 section 3.3 - a KEPT device: one of the account's devices that is
 * not the one that changed the keys.
 *
 * The rotating device sends a `key-rotation` envelope into the account's
 * DeviceGroup at an epoch the removed device never held. A kept device
 * verifies it (one FFI call, `verifyKeyRotationEnvelope`) and stores it as
 * PENDING, changing nothing. It ADOPTS when the server side shows the
 * envelope's key as the account's: the published identity bundle carries
 * it, or this device's own refresh is answered `key-generation-superseded`
 * and the session upgrade signed by the envelope's key is accepted.
 * Adopting swaps the seed and the phrase and keeps every chat, contact and
 * message; the old account key stays as this account's PREDECESSOR
 * ([OwnPredecessorStore]) because the conversation leaves that exist are
 * still bound to it (Stage A).
 *
 * A device told `key-generation-superseded` that holds NO pending envelope
 * reads its DeviceGroup journal to the head ONCE before it treats the
 * change as a reset (the staged wipe and "Was it you?").
 *
 * Everything here is a pure decision or a store sealed under the device
 * key; a host JVM test runs the decisions without a server or the native
 * core.
 */

// ---------------------------------------------------------------------------
// Sealed blobs (the device key seals; tests fake it)
// ---------------------------------------------------------------------------

/** A named plaintext kept sealed under the hardware-backed device key. */
public interface SealedBlobStorage {
    /** The plaintext, or `null` when absent or unreadable. */
    public fun read(name: String): ByteArray?

    /** Store [plaintext]; throws when it could not be sealed or written. */
    public fun write(name: String, plaintext: ByteArray)

    public fun delete(name: String)
}

/** [SealedBlobStorage] on the device: `noBackupFilesDir`, sealed with the
 *  Keystore-resident wrap key (in no backup, never extractable). */
internal class DeviceSealedBlobStorage(private val context: Context, private val dirName: String) : SealedBlobStorage {
    private fun file(name: String): File = File(File(context.noBackupFilesDir, dirName), name)

    override fun read(name: String): ByteArray? {
        val f = file(name)
        if (!f.exists()) return null
        return try {
            DeviceKeyStore.openWithDeviceKey(f.readBytes())
        } catch (_: Throwable) {
            null
        }
    }

    override fun write(name: String, plaintext: ByteArray) {
        val f = file(name)
        f.parentFile?.let { if (!it.exists()) it.mkdirs() }
        f.writeBytes(DeviceKeyStore.sealWithDeviceKey(plaintext))
    }

    override fun delete(name: String) {
        file(name).delete()
    }
}

/** The directory the rotation's sealed files live in, and their names. The
 *  files are named by the account id in HEX; the wipe deletes them by name
 *  ([KeyRotationFiles.deleteAll]). */
public object KeyRotationFiles {
    public const val DIR: String = "lakko-key-rotation"
    public const val PENDING_SUFFIX: String = ".pending"
    public const val PREDECESSOR_SUFFIX: String = ".pred"
    public const val PREPARED_SUFFIX: String = ".prepared"

    public fun hexOf(accountIdB64: String): String? = try {
        base64Decode(accountIdB64).joinToString("") { "%02x".format(it) }
    } catch (_: IllegalArgumentException) {
        null
    }

    public fun onDevice(context: Context): SealedBlobStorage =
        DeviceSealedBlobStorage(context.applicationContext, DIR)

    /** Delete every rotation file of [accountIdB64] without opening one
     *  (the wipe's step; needs no Keystore). */
    public fun deleteAll(context: Context, accountIdB64: String) {
        val hex = hexOf(accountIdB64) ?: return
        val dir = File(context.noBackupFilesDir, DIR)
        for (suffix in listOf(PENDING_SUFFIX, PREDECESSOR_SUFFIX, PREPARED_SUFFIX)) {
            File(dir, hex + suffix).delete()
        }
    }
}

// ---------------------------------------------------------------------------
// Pending envelopes
// ---------------------------------------------------------------------------

/**
 * A verified `key-rotation` envelope a kept device is holding. [newEntropy]
 * IS the new account: sealed under the device key, never logged.
 */
public class PendingRotation(
    /** The account identity key the proof authorises and the entropy
     *  derives: what a pending envelope is keyed on. */
    public val newSignaturePubkey: ByteArray,
    /** The 16 bytes of BIP39 entropy of the new recovery phrase. */
    public val newEntropy: ByteArray,
    /** The 145-byte supersession proof, old account key over new. */
    public val supersessionProof: ByteArray,
    /** The rotating device's clock when it prepared the envelope. Ages the
     *  pending envelope out; orders and authorises nothing. */
    public val preparedAtMs: Long,
    public val proofExpiresAtUnixSecs: Long,
) {
    public fun zero() {
        newEntropy.fill(0)
    }

    override fun toString(): String = "PendingRotation(<redacted>)"
}

/** `version(1) | count(1) | count x [pubkey(32) | entropy(16) | proof(145) |
 *  prepared_at_ms(8 BE) | proof_expires_at_secs(8 BE)]`. */
public object PendingRotationCodec {
    public const val VERSION: Byte = 1
    public const val PUBKEY_LEN: Int = 32
    public const val ENTROPY_LEN: Int = 16
    public const val PROOF_LEN: Int = 145
    private const val ROW_LEN: Int = PUBKEY_LEN + ENTROPY_LEN + PROOF_LEN + 8 + 8

    public fun encode(rows: List<PendingRotation>): ByteArray {
        require(rows.size <= MAX_PENDING_ROTATIONS) { "too many pending rotations" }
        val buf = ByteBuffer.allocate(2 + rows.size * ROW_LEN).put(VERSION).put(rows.size.toByte())
        for (row in rows) {
            require(row.newSignaturePubkey.size == PUBKEY_LEN) { "pubkey must be $PUBKEY_LEN bytes" }
            require(row.newEntropy.size == ENTROPY_LEN) { "entropy must be $ENTROPY_LEN bytes" }
            require(row.supersessionProof.size == PROOF_LEN) { "proof must be $PROOF_LEN bytes" }
            buf.put(row.newSignaturePubkey).put(row.newEntropy).put(row.supersessionProof)
                .putLong(row.preparedAtMs).putLong(row.proofExpiresAtUnixSecs)
        }
        return buf.array()
    }

    /** `null` for anything that is not exactly one list. */
    public fun decode(bytes: ByteArray): List<PendingRotation>? {
        if (bytes.size < 2 || bytes[0] != VERSION) return null
        val count = bytes[1].toInt() and 0xFF
        if (count > MAX_PENDING_ROTATIONS || bytes.size != 2 + count * ROW_LEN) return null
        val buf = ByteBuffer.wrap(bytes, 2, bytes.size - 2)
        return List(count) {
            PendingRotation(
                newSignaturePubkey = ByteArray(PUBKEY_LEN).also { buf.get(it) },
                newEntropy = ByteArray(ENTROPY_LEN).also { buf.get(it) },
                supersessionProof = ByteArray(PROOF_LEN).also { buf.get(it) },
                preparedAtMs = buf.long,
                proofExpiresAtUnixSecs = buf.long,
            )
        }
    }
}

/** A device holds at most this many pending envelopes (a legitimate account
 *  prepares one at a time; the bound keeps a flood of prepared-and-abandoned
 *  rotations from growing the store). */
public const val MAX_PENDING_ROTATIONS: Int = 8

/** [rows] with [incoming] stored: keyed on the new key, so the same
 *  envelope seen again replaces its row; past the cap the oldest goes. */
public fun withPendingRotation(rows: List<PendingRotation>, incoming: PendingRotation): List<PendingRotation> {
    val others = rows.filterNot { it.newSignaturePubkey.contentEquals(incoming.newSignaturePubkey) }
    return (others + incoming).sortedBy { it.preparedAtMs }.takeLast(MAX_PENDING_ROTATIONS)
}

/** [rows] without the ones [expired] says are over (14 days,
 *  `keyRotationPendingExpired`). */
public fun withoutExpiredRotations(
    rows: List<PendingRotation>,
    nowMs: Long,
    expired: (preparedAtMs: Long, nowMs: Long) -> Boolean,
): List<PendingRotation> = rows.filterNot { expired(it.preparedAtMs, nowMs) }

/** The pending envelopes of each account, sealed under the device key. */
public class PendingRotationStore(private val storage: SealedBlobStorage) {
    public fun load(accountIdB64: String): List<PendingRotation> {
        val name = nameOf(accountIdB64) ?: return emptyList()
        val plain = storage.read(name) ?: return emptyList()
        return try {
            PendingRotationCodec.decode(plain).orEmpty()
        } finally {
            plain.fill(0)
        }
    }

    /** Persist [rows]; `false` when they could not be stored. */
    public fun save(accountIdB64: String, rows: List<PendingRotation>): Boolean {
        val name = nameOf(accountIdB64) ?: return false
        if (rows.isEmpty()) {
            storage.delete(name)
            return true
        }
        val plain = PendingRotationCodec.encode(rows)
        return try {
            storage.write(name, plain)
            true
        } catch (_: Throwable) {
            false
        } finally {
            plain.fill(0)
        }
    }

    public fun clear(accountIdB64: String) {
        nameOf(accountIdB64)?.let(storage::delete)
    }

    private fun nameOf(accountIdB64: String): String? =
        KeyRotationFiles.hexOf(accountIdB64)?.let { it + KeyRotationFiles.PENDING_SUFFIX }

    public companion object {
        public fun onDevice(context: Context): PendingRotationStore =
            PendingRotationStore(KeyRotationFiles.onDevice(context))
    }
}

// ---------------------------------------------------------------------------
// The kept device's verdicts
// ---------------------------------------------------------------------------

/** What a device does about a sign that the account's keys moved. */
public sealed interface KeptDeviceVerdict {
    /** The published bundle carries this pending envelope's key: adopt it. */
    public class Adopt(public val pending: PendingRotation) : KeptDeviceVerdict

    /** The refresh was superseded and the server did not say by which key:
     *  ask for the session upgrade with each pending envelope's key, newest
     *  first; the one the server accepts is the account's. */
    public class TryUpgrade(public val candidates: List<PendingRotation>) : KeptDeviceVerdict

    /** Nothing pending (or nothing that matches) and the DeviceGroup journal
     *  was not read to its head yet: read it, once, then decide again. */
    public data object ReadJournalOnce : KeptDeviceVerdict

    /** No envelope explains the change: a reset (the staged wipe and "Was
     *  it you?"). */
    public data object TreatAsReset : KeptDeviceVerdict

    /** Nothing to do now: no sign that this device's keys are stale. */
    public data object Nothing : KeptDeviceVerdict
}

/**
 * The kept device's decision.
 *
 * @param pending the envelopes held (already aged: expired ones dropped).
 * @param publishedKey the key the directory serves, when it was read.
 * @param superseded this device's refresh (or a bearer route) answered
 *   `key-generation-superseded`, or the directory serves another key at a
 *   higher generation.
 * @param journalRead the DeviceGroup journal was read to its head since
 *   the sign appeared.
 */
public fun keptDeviceVerdict(
    pending: List<PendingRotation>,
    publishedKey: ByteArray?,
    superseded: Boolean,
    journalRead: Boolean,
): KeptDeviceVerdict {
    if (publishedKey != null) {
        // The bundle carries a pending envelope's key: that is the commit.
        pending.firstOrNull { it.newSignaturePubkey.contentEquals(publishedKey) }
            ?.let { return KeptDeviceVerdict.Adopt(it) }
    }
    // Nothing says this device's session is over: an envelope alone changes
    // nothing (its key may never become the account's).
    if (!superseded) return KeptDeviceVerdict.Nothing
    // Superseded, and the published key (if read) matches no envelope. That
    // is not proof of a reset yet (the directory upload trails the commit):
    // the server's answer to the upgrade decides.
    if (pending.isNotEmpty()) {
        return KeptDeviceVerdict.TryUpgrade(pending.sortedByDescending { it.preparedAtMs })
    }
    return if (journalRead) KeptDeviceVerdict.TreatAsReset else KeptDeviceVerdict.ReadJournalOnce
}

/** What `POST /auth/session/refresh` with a key statement answered. */
public sealed interface UpgradeOutcome {
    /** A pair at the new generation naming this device. */
    public class Upgraded(public val pair: SessionPair) : UpgradeOutcome

    /** `key-generation-superseded`: the statement was not accepted (this
     *  key is not the account's). */
    public data object NotAccepted : UpgradeOutcome

    /** `device-revoked`: the key IS the account's and this device was the
     *  one removed. The full wipe. */
    public data object DeviceRevoked : UpgradeOutcome

    /** `refresh-invalid`: the credential itself is gone; sign in again,
     *  nothing wiped. */
    public data object RefreshInvalid : UpgradeOutcome

    /** No usable answer (offline, a limit, a server fault, a clock the
     *  retry did not fix): nothing is decided; ask again later. */
    public class Unanswered(public val cause: Throwable) : UpgradeOutcome
}

/** Classify a failed upgrade. `statement-stale` that survived its one
 *  re-signed retry is [UpgradeOutcome.Unanswered]: never a reason to wipe. */
public fun upgradeOutcomeOf(err: Throwable): UpgradeOutcome = when {
    err is AuthHttpError && statementStaleServerTimeMs(err) != null -> UpgradeOutcome.Unanswered(err)
    err is AuthHttpError && err.status == KEPT_HTTP_UNAUTHORIZED && err.code == STATEMENT_STALE ->
        UpgradeOutcome.Unanswered(err)
    err is AuthHttpError -> when (sessionRefusalOf(err.status, err.code)) {
        SessionRefusal.KeysReset -> UpgradeOutcome.NotAccepted
        SessionRefusal.DeviceRevoked -> UpgradeOutcome.DeviceRevoked
        SessionRefusal.RefreshInvalid -> UpgradeOutcome.RefreshInvalid
        // An unlabelled 401 decides nothing here.
        SessionRefusal.Unauthorized, null -> UpgradeOutcome.Unanswered(err)
    }
    else -> UpgradeOutcome.Unanswered(err)
}

/** How the trials of [KeptDeviceVerdict.TryUpgrade] ended. */
public sealed interface KeptResolution {
    /** The server accepted [pending]'s key: adopt with [pair]. */
    public class Adopted(public val pending: PendingRotation, public val pair: SessionPair) : KeptResolution

    /** This device was the one removed: the removed screen, the full wipe. */
    public data object Removed : KeptResolution

    /** The refresh credential is not accepted: the sign-in screen. */
    public data object SignInAgain : KeptResolution

    /** The server accepted none of the keys held: a reset after all (or a
     *  rotation whose envelope this device never read). */
    public data object Reset : KeptResolution

    /** At least one trial had no usable answer: nothing is wiped, nothing
     *  adopted; the device stays as it is ("Updating keys...") and asks
     *  again. */
    public data object Later : KeptResolution
}

/**
 * Fold the trials, in the order they were made. An accepted key wins at
 * once; "removed" and "sign in again" are about the credential and end the
 * trials; a trial without an answer keeps the device from concluding
 * "reset" (it must not wipe on a failed request).
 */
public fun keptResolutionOf(trials: List<Pair<PendingRotation, UpgradeOutcome>>): KeptResolution {
    var unanswered = false
    for ((pending, outcome) in trials) {
        when (outcome) {
            is UpgradeOutcome.Upgraded -> return KeptResolution.Adopted(pending, outcome.pair)
            UpgradeOutcome.DeviceRevoked -> return KeptResolution.Removed
            UpgradeOutcome.RefreshInvalid -> return KeptResolution.SignInAgain
            UpgradeOutcome.NotAccepted -> Unit
            is UpgradeOutcome.Unanswered -> unanswered = true
        }
    }
    return if (unanswered) KeptResolution.Later else KeptResolution.Reset
}

/** Whether an upgraded pair really is of a later generation than the one it
 *  replaced: a credential that was NOT of a lower generation is refreshed
 *  without the statement being read, which proves nothing about the key. */
public fun upgradeProvesKey(previousGeneration: Long, upgraded: SessionPair): Boolean =
    upgraded.keyGeneration > previousGeneration

// ---------------------------------------------------------------------------
// This account's own predecessors
// ---------------------------------------------------------------------------

/**
 * A previous account identity of THIS account, kept after a rotation the
 * account made itself. The conversation leaves that exist are bound to its
 * key until Stage B moves them, Welcomes for those bindings arrive in its
 * mailbox, and journals it rooted are handed over by it.
 */
public class OwnPredecessor(
    /** The old account identity's Ed25519 public key. */
    public val signaturePubkey: ByteArray,
    /** The old 64-byte phrase seed (`Identity.fromSeed`). */
    public val phraseSeed: ByteArray,
    /** When this device adopted the successor (unix ms). */
    public val supersededAtMs: Long,
    /** The old account mailbox's read cursor on this device. */
    public val mailboxCursor: Long = 0,
) {
    public fun zero() {
        phraseSeed.fill(0)
    }

    override fun toString(): String = "OwnPredecessor(<redacted>)"
}

/** `version(1) | count(1) | count x [pubkey(32) | seed(64) | at_ms(8 BE) |
 *  cursor(8 BE)]`, earliest first. */
public object OwnPredecessorCodec {
    public const val VERSION: Byte = 1
    public const val SEED_LEN: Int = 64
    private const val PUBKEY_LEN: Int = 32
    private const val ROW_LEN: Int = PUBKEY_LEN + SEED_LEN + 8 + 8

    public fun encode(rows: List<OwnPredecessor>): ByteArray {
        require(rows.size <= MAX_OWN_PREDECESSORS) { "too many predecessors" }
        val buf = ByteBuffer.allocate(2 + rows.size * ROW_LEN).put(VERSION).put(rows.size.toByte())
        for (row in rows) {
            require(row.signaturePubkey.size == PUBKEY_LEN) { "pubkey must be $PUBKEY_LEN bytes" }
            require(row.phraseSeed.size == SEED_LEN) { "seed must be $SEED_LEN bytes" }
            buf.put(row.signaturePubkey).put(row.phraseSeed).putLong(row.supersededAtMs).putLong(row.mailboxCursor)
        }
        return buf.array()
    }

    public fun decode(bytes: ByteArray): List<OwnPredecessor>? {
        if (bytes.size < 2 || bytes[0] != VERSION) return null
        val count = bytes[1].toInt() and 0xFF
        if (count > MAX_OWN_PREDECESSORS || bytes.size != 2 + count * ROW_LEN) return null
        val buf = ByteBuffer.wrap(bytes, 2, bytes.size - 2)
        return List(count) {
            OwnPredecessor(
                signaturePubkey = ByteArray(PUBKEY_LEN).also { buf.get(it) },
                phraseSeed = ByteArray(SEED_LEN).also { buf.get(it) },
                supersededAtMs = buf.long,
                mailboxCursor = buf.long,
            )
        }
    }
}

/** An account rotates rarely; past this many the earliest is still kept
 *  (it is the one the conversation leaves name) and the middle ones go. */
public const val MAX_OWN_PREDECESSORS: Int = 8

/** [rows] with [added] as the newest predecessor. Idempotent on the key. */
public fun withOwnPredecessor(rows: List<OwnPredecessor>, added: OwnPredecessor): List<OwnPredecessor> {
    if (rows.any { it.signaturePubkey.contentEquals(added.signaturePubkey) }) return rows
    val next = rows + added
    if (next.size <= MAX_OWN_PREDECESSORS) return next
    return listOf(next.first()) + next.drop(2)
}

/** This account's own previous identities on this device, earliest first,
 *  sealed under the device key. */
public class OwnPredecessorStore(private val storage: SealedBlobStorage) {
    public fun load(accountIdB64: String): List<OwnPredecessor> {
        val name = nameOf(accountIdB64) ?: return emptyList()
        val plain = storage.read(name) ?: return emptyList()
        return try {
            OwnPredecessorCodec.decode(plain).orEmpty()
        } finally {
            plain.fill(0)
        }
    }

    /** The identity the conversation leaves that exist are bound to: the
     *  EARLIEST predecessor, or `null` when the account never rotated. */
    public fun conversationSeed(accountIdB64: String): ByteArray? {
        val rows = load(accountIdB64)
        val first = rows.firstOrNull()?.phraseSeed?.copyOf()
        rows.forEach { it.zero() }
        return first
    }

    public fun add(accountIdB64: String, added: OwnPredecessor): Boolean =
        save(accountIdB64, withOwnPredecessor(load(accountIdB64), added))

    public fun save(accountIdB64: String, rows: List<OwnPredecessor>): Boolean {
        val name = nameOf(accountIdB64) ?: return false
        if (rows.isEmpty()) {
            storage.delete(name)
            return true
        }
        val plain = OwnPredecessorCodec.encode(rows)
        return try {
            storage.write(name, plain)
            true
        } catch (_: Throwable) {
            false
        } finally {
            plain.fill(0)
        }
    }

    /** Move the old mailbox's cursor forward (never backwards). */
    public fun advanceMailboxCursor(accountIdB64: String, signaturePubkey: ByteArray, cursor: Long) {
        val rows = load(accountIdB64)
        var changed = false
        val next = rows.map { row ->
            if (row.signaturePubkey.contentEquals(signaturePubkey) && cursor > row.mailboxCursor) {
                changed = true
                OwnPredecessor(row.signaturePubkey, row.phraseSeed, row.supersededAtMs, cursor)
            } else {
                row
            }
        }
        if (changed) save(accountIdB64, next)
        rows.forEach { it.zero() }
    }

    public fun clear(accountIdB64: String) {
        nameOf(accountIdB64)?.let(storage::delete)
    }

    private fun nameOf(accountIdB64: String): String? =
        KeyRotationFiles.hexOf(accountIdB64)?.let { it + KeyRotationFiles.PREDECESSOR_SUFFIX }

    public companion object {
        public fun onDevice(context: Context): OwnPredecessorStore =
            OwnPredecessorStore(KeyRotationFiles.onDevice(context))
    }
}

/**
 * The 64-byte seed of the account identity the conversation leaves that
 * exist are bound to (ADR-0066 section 3.1, Stage A): the EARLIEST own
 * predecessor after a rotation, else [currentSeed]. Everything that speaks
 * AS a member of an existing MLS group (the chat identity's binding, a call
 * founded for a link, a channel's PublisherGroup) uses this; everything the
 * directory, auth and the mailbox of the published key see uses the current
 * seed. Always a fresh copy.
 */
public fun conversationAccountSeedOf(context: Context, accountIdB64: String, currentSeed: ByteArray): ByteArray =
    try {
        OwnPredecessorStore.onDevice(context).conversationSeed(accountIdB64)
    } catch (_: Throwable) {
        null
    } ?: currentSeed.copyOf()

// ---------------------------------------------------------------------------
// The rotating device's prepared material
// ---------------------------------------------------------------------------

/**
 * What the rotating device generated before its commit: the new phrase's
 * entropy and the old key's proof over the new key. Kept sealed until the
 * rotation is done or abandoned, so a lost `rotate/finish` answer never
 * loses the keys the account may already be at.
 */
public class PreparedRotation(
    public val newEntropy: ByteArray,
    public val supersessionProof: ByteArray,
    public val preparedAtMs: Long,
) {
    public fun zero() {
        newEntropy.fill(0)
    }

    override fun toString(): String = "PreparedRotation(<redacted>)"
}

/** `version(1) | entropy(16) | proof(145) | prepared_at_ms(8 BE)`. */
public object PreparedRotationCodec {
    public const val VERSION: Byte = 1
    private const val LEN: Int = 1 + PendingRotationCodec.ENTROPY_LEN + PendingRotationCodec.PROOF_LEN + 8

    public fun encode(prepared: PreparedRotation): ByteArray {
        require(prepared.newEntropy.size == PendingRotationCodec.ENTROPY_LEN) { "entropy length" }
        require(prepared.supersessionProof.size == PendingRotationCodec.PROOF_LEN) { "proof length" }
        return ByteBuffer.allocate(LEN).put(VERSION).put(prepared.newEntropy).put(prepared.supersessionProof)
            .putLong(prepared.preparedAtMs).array()
    }

    public fun decode(bytes: ByteArray): PreparedRotation? {
        if (bytes.size != LEN || bytes[0] != VERSION) return null
        val buf = ByteBuffer.wrap(bytes, 1, bytes.size - 1)
        return PreparedRotation(
            newEntropy = ByteArray(PendingRotationCodec.ENTROPY_LEN).also { buf.get(it) },
            supersessionProof = ByteArray(PendingRotationCodec.PROOF_LEN).also { buf.get(it) },
            preparedAtMs = buf.long,
        )
    }
}

/** How long a rotation's supersession proof is valid (ADR-0066 section
 *  3.2: 14 days, as before ADR-0017). */
public const val ROTATION_PROOF_VALIDITY_SECS: Long = 14L * 24 * 60 * 60

/** A prepared proof with less than this left is replaced before a commit
 *  (chosen: a day of margin for the publish to land). */
public const val ROTATION_PROOF_MIN_REMAINING_SECS: Long = 24L * 60 * 60

/** Whether prepared material made at [preparedAtMs] can still be committed
 *  at [nowMs]. A clock that went backwards reads as "no". */
public fun preparedRotationUsable(preparedAtMs: Long, nowMs: Long): Boolean {
    if (nowMs < preparedAtMs) return false
    val ageSecs = (nowMs - preparedAtMs) / 1000
    return ageSecs <= ROTATION_PROOF_VALIDITY_SECS - ROTATION_PROOF_MIN_REMAINING_SECS
}

/** The prepared material of each account, sealed under the device key. */
public class PreparedRotationStore(private val storage: SealedBlobStorage) {
    public fun load(accountIdB64: String): PreparedRotation? {
        val name = nameOf(accountIdB64) ?: return null
        val plain = storage.read(name) ?: return null
        return try {
            PreparedRotationCodec.decode(plain)
        } finally {
            plain.fill(0)
        }
    }

    /** Persist [prepared] and read it back; `false` when it is not there. */
    public fun save(accountIdB64: String, prepared: PreparedRotation): Boolean {
        val name = nameOf(accountIdB64) ?: return false
        val plain = PreparedRotationCodec.encode(prepared)
        return try {
            storage.write(name, plain)
            val again = storage.read(name)
            val ok = again != null && again.contentEquals(plain)
            again?.fill(0)
            ok
        } catch (_: Throwable) {
            false
        } finally {
            plain.fill(0)
        }
    }

    public fun clear(accountIdB64: String) {
        nameOf(accountIdB64)?.let(storage::delete)
    }

    private fun nameOf(accountIdB64: String): String? =
        KeyRotationFiles.hexOf(accountIdB64)?.let { it + KeyRotationFiles.PREPARED_SUFFIX }

    public companion object {
        public fun onDevice(context: Context): PreparedRotationStore =
            PreparedRotationStore(KeyRotationFiles.onDevice(context))
    }
}

private const val KEPT_HTTP_UNAUTHORIZED: Int = 401
