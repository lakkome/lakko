// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.nio.ByteBuffer
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.lakko_ffi_uniffi.FfiBindingVerdict
import uniffi.lakko_ffi_uniffi.FfiOwnLink

/*
 * G-56 (ADR-0074, "Joining after a rotation: the device waits for the earlier
 * keys", 2026-11-15; UX docs/ux/identity-recovery.md section 8a).
 *
 * A device that joins an account binds its device identity under the
 * account's CONVERSATION key, the oldest own predecessor after a rotation. It
 * must not bind, publish a KeyPackage, join a conversation or open the
 * DeviceGroup while the account's own conversation link names a key it does
 * not hold: it parks instead ([AuthState.NeedsEarlierKeys]) and looks for the
 * earlier keys by itself.
 *
 * The verdict and the link rule are the core's (`ownBindingVerdict`,
 * `ownConversationKey`); this file orders the calls and keeps what was read.
 * Every native call is a port, so the host JVM tests drive the ordering with
 * fakes.
 */

/** Thrown by the chat layer's mint when the core says `wait`: nothing was
 *  minted, stored or published (defence in depth behind the auth gate). */
public class EarlierKeysMissing : IllegalStateException("the account's earlier keys are not on this device")

/** How often the parked screen looks for the earlier keys while it is in the
 *  foreground (chosen, not measured; ADR-0074 G-56 rule 6). */
public const val EARLIER_KEYS_RETRY_MS: Long = 5L * 60L * 1000L

/** Whether the core keeps the stored binding (no mint, no link needed). */
public fun FfiBindingVerdict.isKept(): Boolean =
    this is FfiBindingVerdict.Keep || this is FfiBindingVerdict.KeepUnderCurrentKey

/**
 * The link answer of a fetched identity bundle. [linkB64] is the bundle's
 * `conversation_link` (standard base64), [currentKey] THIS device's own
 * current account key (never the fetched one), [ownConversationKey] the
 * core's rule. Absent, empty, undecodable, a link that does not verify, and
 * one naming the current key itself all read as [FfiOwnLink.Absent].
 */
public fun ownLinkOf(
    linkB64: String?,
    currentKey: ByteArray,
    ownConversationKey: (ByteArray?, ByteArray) -> ByteArray?,
): FfiOwnLink {
    if (linkB64.isNullOrEmpty()) return FfiOwnLink.Absent
    val link = try {
        base64Decode(linkB64)
    } catch (_: IllegalArgumentException) {
        return FfiOwnLink.Absent
    }
    val key = try {
        ownConversationKey(link, currentKey)
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        null
    } ?: return FfiOwnLink.Absent
    if (key.contentEquals(currentKey)) return FfiOwnLink.Absent
    return FfiOwnLink.Key(key)
}

/** The link as the local record last saw it: `Absent` when there is no record
 *  or it named no key. */
public fun linkOfRecord(record: OwnConversationKeyRecord?): FfiOwnLink =
    record?.conversationKey?.let { FfiOwnLink.Key(it.copyOf()) } ?: FfiOwnLink.Absent

// ---------------------------------------------------------------------------
// The local record (contract section 1.3)
// ---------------------------------------------------------------------------

/** What this device last read of the account's own conversation link: the
 *  conversation key, or `null` for "none", and when it was read. */
public class OwnConversationKeyRecord(
    public val conversationKey: ByteArray?,
    public val readAtMs: Long,
)

/** `version(1) | present(1) | key(32 when present) | read_at_ms(8 BE)`. */
public object OwnConversationKeyCodec {
    public const val VERSION: Byte = 1
    private const val KEY_LEN: Int = 32

    public fun encode(record: OwnConversationKeyRecord): ByteArray {
        val key = record.conversationKey
        require(key == null || key.size == KEY_LEN) { "conversation key must be $KEY_LEN bytes" }
        val buf = ByteBuffer.allocate(2 + (key?.size ?: 0) + 8).put(VERSION).put(if (key == null) 0 else 1)
        if (key != null) buf.put(key)
        return buf.putLong(record.readAtMs).array()
    }

    public fun decode(bytes: ByteArray): OwnConversationKeyRecord? {
        if (bytes.size < 2 || bytes[0] != VERSION) return null
        return when (bytes[1].toInt()) {
            0 -> if (bytes.size != 10) null else OwnConversationKeyRecord(null, ByteBuffer.wrap(bytes, 2, 8).long)
            1 -> if (bytes.size != 2 + KEY_LEN + 8) {
                null
            } else {
                OwnConversationKeyRecord(bytes.copyOfRange(2, 2 + KEY_LEN), ByteBuffer.wrap(bytes, 2 + KEY_LEN, 8).long)
            }
            else -> null
        }
    }
}

/** The record of each account, sealed under the device key beside the own
 *  predecessors; deleted by Forget device ([KeyRotationFiles.deleteAll]) and
 *  by a key reset or recall on this device. */
public class OwnConversationKeyStore(private val storage: SealedBlobStorage) {
    public fun load(accountIdB64: String): OwnConversationKeyRecord? {
        val name = nameOf(accountIdB64) ?: return null
        val plain = storage.read(name) ?: return null
        return try {
            OwnConversationKeyCodec.decode(plain)
        } finally {
            plain.fill(0)
        }
    }

    /** Store what was read; `false` when it is not stored (a signed-out or
     *  wiped account refuses the write). */
    public fun save(accountIdB64: String, record: OwnConversationKeyRecord): Boolean {
        val name = nameOf(accountIdB64) ?: return false
        val plain = OwnConversationKeyCodec.encode(record)
        return try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.write(name, plain) } != null
        } catch (_: Throwable) {
            false
        }
    }

    public fun clear(accountIdB64: String) {
        nameOf(accountIdB64)?.let(storage::delete)
    }

    private fun nameOf(accountIdB64: String): String? =
        KeyRotationFiles.hexOf(accountIdB64)?.let { it + KeyRotationFiles.CONVERSATION_KEY_SUFFIX }

    public companion object {
        public fun onDevice(context: android.content.Context): OwnConversationKeyStore =
            OwnConversationKeyStore(KeyRotationFiles.onDevice(context))
    }
}

/** The record to write for [link]: `null` for [FfiOwnLink.Unknown], which
 *  says nothing and leaves the record as it was. */
public fun recordOfLink(link: FfiOwnLink, nowMs: Long): OwnConversationKeyRecord? = when (link) {
    is FfiOwnLink.Key -> OwnConversationKeyRecord(link.key.copyOf(), nowMs)
    FfiOwnLink.Absent -> OwnConversationKeyRecord(null, nowMs)
    else -> null
}

// ---------------------------------------------------------------------------
// The gate (contract section 2)
// ---------------------------------------------------------------------------

/** What the gate reads and calls. Production: the stores and the core;
 *  tests: fakes. Keys are raw 32-byte account keys. */
public interface EarlierKeysGatePorts {
    /** The account key the stored device binding names, `null` when there is
     *  none or it does not verify. */
    public fun boundKey(): ByteArray?

    /** This device's current account key (from the seed it signed in with). */
    public fun currentKey(): ByteArray

    /** The keys of the own predecessor seeds held, oldest first. */
    public fun predecessorKeys(): List<ByteArray>

    /** Whether this device holds any conversation. */
    public fun holdsConversations(): Boolean

    /** The core's `ownBindingVerdict`. */
    public fun verdict(
        boundKey: ByteArray?,
        currentKey: ByteArray,
        predecessorKeys: List<ByteArray>,
        link: FfiOwnLink,
        holdsConversations: Boolean,
    ): FfiBindingVerdict

    /** What the local record last said ([linkOfRecord]); `Absent` with no
     *  record. */
    public fun recordedLink(): FfiOwnLink = FfiOwnLink.Absent

    /** Read the account's own bundle and answer its link
     *  ([ownLinkOf]); [FfiOwnLink.Unknown] when it could not be read. */
    public suspend fun readLink(): FfiOwnLink

    /** Keep what was read ([recordOfLink]). */
    public fun recordLink(link: FfiOwnLink)
}

/** The gate's answer. */
public sealed interface EarlierKeysDecision {
    /** Land: the stored binding is kept, or a mint under a held key is due. */
    public class Land(public val verdict: FfiBindingVerdict) : EarlierKeysDecision

    /** Park in [AuthState.NeedsEarlierKeys]: bind, publish and open nothing. */
    public data object Park : EarlierKeysDecision
}

/**
 * The pre-landing gate. The verdict is taken first with the link as the local
 * record last saw it (`Absent` with no record); a kept binding lands with no
 * request. A record naming a key this device lacks is how a device that
 * landed before it learned the link parks at its next landing. Otherwise the link is read ([prefetched]
 * when the route already fetched the bundle, else [EarlierKeysGatePorts.readLink])
 * and recorded, and the verdict taken again: `wait` parks, anything else
 * lands. Every port failure lands on the side of waiting: an unreadable
 * verdict parks, an unreadable bundle reads `Unknown` (and the core waits).
 */
public suspend fun runEarlierKeysGate(
    ports: EarlierKeysGatePorts,
    prefetched: FfiOwnLink? = null,
): EarlierKeysDecision {
    val bound = ports.boundKey()
    val current = ports.currentKey()
    val predecessors = ports.predecessorKeys()
    val holds = ports.holdsConversations()
    val first = try {
        ports.verdict(bound, current, predecessors, ports.recordedLink(), holds)
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        return EarlierKeysDecision.Park
    }
    if (first.isKept()) return EarlierKeysDecision.Land(first)
    val link = prefetched ?: try {
        ports.readLink()
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        FfiOwnLink.Unknown
    }
    ports.recordLink(link)
    val verdict = try {
        ports.verdict(bound, current, predecessors, link, holds)
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        return EarlierKeysDecision.Park
    }
    return if (verdict is FfiBindingVerdict.Wait) EarlierKeysDecision.Park else EarlierKeysDecision.Land(verdict)
}

/**
 * One look for the earlier keys while parked (contract section 4.2): the
 * backup is fetched with the phrase this device holds and its own
 * predecessors are stored ([fetchBackupPredecessors], best effort, its
 * failure not shown), then the gate runs again with a fresh read of the
 * link. `true` when the device may land now.
 */
public suspend fun lookForEarlierKeys(
    fetchBackupPredecessors: suspend () -> Unit,
    ports: EarlierKeysGatePorts,
): Boolean {
    try {
        fetchBackupPredecessors()
    } catch (e: CancellationException) {
        throw e
    } catch (_: Exception) {
        // A failed look is not shown; the link may still have gone.
    }
    return runEarlierKeysGate(ports, prefetched = null) is EarlierKeysDecision.Land
}

/**
 * The join-state write of a device restore with the account's earlier keys
 * stored FIRST (contract section 3.3 step 4): nothing the join writes, and no
 * landing after it, can find the device without them.
 */
public fun predecessorsFirst(storePredecessors: () -> Unit, persistJoin: suspend () -> Unit): suspend () -> Unit = {
    storePredecessors()
    persistJoin()
}

/**
 * At most one landing at a time for a parked account: the device's own look
 * for the earlier keys and a completion the person started ("Another device",
 * the phrase) never both land. Whoever takes the lock second re-checks
 * [runIfParked]'s condition and stands down when the other already landed.
 */
public class ParkedLandingGuard {
    private val mutex = Mutex()

    /** [land] under the lock when [stillParked] still holds once the lock is
     *  taken; `null` (nothing run) otherwise. */
    public suspend fun <T> runIfParked(stillParked: () -> Boolean, land: suspend () -> T): T? =
        mutex.withLock { if (stillParked()) land() else null }
}

// ---------------------------------------------------------------------------
// The DeviceGroup lineage guard (contract section 2.4)
// ---------------------------------------------------------------------------

/**
 * Whether the DeviceGroup lineage seed whose key is [lineageKey] may name the
 * journal: always without a recorded conversation key, else only when it
 * derives exactly that key. A device that holds the current seed alone and a
 * record naming an earlier key opens, founds and knocks nothing.
 */
public fun lineageFollowsLink(recordedConversationKey: ByteArray?, lineageKey: ByteArray): Boolean =
    recordedConversationKey == null || recordedConversationKey.contentEquals(lineageKey)
