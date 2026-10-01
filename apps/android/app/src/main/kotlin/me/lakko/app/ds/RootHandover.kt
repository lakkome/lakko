// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import java.util.concurrent.ConcurrentHashMap

/*
 * ADR-0057 "What the S2 client work found", item 1 — an ownership transfer
 * moves the journal root.
 *
 * ADR-0045 ownership transfer rewrites the role table, not the journal: the
 * Delivery Service still knows the OLD owner's account-derived root, and the
 * old owner cannot name the new owner's root (that key is derived from the new
 * owner's account seed). So the old owner hands its root seed over:
 *
 *   * Sending (outgoing owner). When a device of this account applies a remote
 *     commit or merges its own sweep, and the role table's owner was this
 *     account BEFORE and another account AFTER, the device seals
 *     `dsJournalRootSeed(ownAccountSeed, gid)` to the new owner's account
 *     X25519 key (`composeRootHandoverEnvelope`, the registrar-seed envelope
 *     at rank root) and posts it in the group. It stops acting as root at the
 *     same moment without anything here doing so: the repository resolves the
 *     journal authority off the LIVE role table (`ownsJournal`), so no
 *     appointment, root registration, claim or reset is signed with the root
 *     afterwards. Every device of the outgoing account may send it. Since the
 *     second security pass (finding 4) the owed record is kept until this
 *     account's own probe ([probeRoot]: `root` naming its own root, signed by
 *     it) answers 401, i.e. the DS no longer knows it as the root; while the
 *     probe answers 204 the envelope is re-sent, at most every
 *     [HANDOVER_RESEND_MS] ([runOwedHandover]).
 *   * Receiving (new owner). A rank-root envelope is kept only when it names
 *     this journal, is addressed to this account and the live role table AT
 *     RECEIPT names this account owner (C-68). No sender check: the DS is the
 *     check, because a seed that is not the current root fails the `root`
 *     statement with 401. The opened seed signs `root` naming this account's
 *     own derived root; after a 204 the new root sends `registrars-set` with
 *     the whole set the live roster calls for (every old registrar not listed
 *     goes with it) and the normal appointment reconcile re-sends every seed.
 *     Until the 204 (or this account's probe) confirms it, this account keeps
 *     signing with its registrar seed (the root-held marker). The opened seed
 *     is kept wrapped under the device key ([PendingRootSeedStore]) until the
 *     takeover lands or answers 401, never enters the registrar-seed store and
 *     is never logged.
 *
 * Everything here takes its signers, the core codec and the network through
 * function parameters, so the host JVM tests drive every rule with fakes.
 */

// -----------------------------------------------------------------
// Detecting an owner change at commit apply / sweep merge
// -----------------------------------------------------------------

/**
 * The pure sending rule: the new owner when the role table's owner was [own]
 * [before] one commit apply / sweep merge and is ANOTHER account [after] it,
 * else `null` (a no-op commit, this account staying owner, becoming owner, a
 * table-less group or an unreadable table).
 */
public fun ownerMovedAwayFrom(own: ByteArray?, before: ByteArray?, after: ByteArray?): ByteArray? {
    if (own == null || before == null || after == null) return null
    if (!before.contentEquals(own)) return null
    if (after.contentEquals(own)) return null
    return after
}

// -----------------------------------------------------------------
// Sending (outgoing owner)
// -----------------------------------------------------------------

/** What one handover attempt did. */
public enum class HandoverSendOutcome {
    /** Posted in the group. */
    SENT,

    /** Nothing to hand over any more: this account is owner again, the group
     *  has no owner, or the session ended. */
    CANCELLED,

    /** The new owner's X25519 key is not resolvable yet: retried later. */
    PENDING,

    /** Composing or sending failed: retried later. */
    FAILED,
}

/**
 * One handover attempt. The recipient is the owner the LIVE role table names
 * now (not the one recorded when the change was seen), so a second transfer in
 * between hands the root to whoever holds the group. Never sent while this
 * account is owner. The root seed [rootSeed] returns is wiped here.
 *
 * @param liveOwner the role table's owner as it stands now, `null` for none
 *   or an unreadable table.
 * @param ownerX25519 the new owner's account X25519 key from its identity
 *   bundle (verified to belong to that account key), or `null` when it does not
 *   resolve yet.
 * @param compose `composeRootHandoverEnvelope(gid, newOwner, x25519, rootSeed)`.
 * @param sendEnvelope seal and publish into the group (an MLS application
 *   message, under the group's MLS lock).
 */
public suspend fun sendRootHandover(
    ownAccountKey: () -> ByteArray?,
    liveOwner: suspend () -> ByteArray?,
    ownerX25519: suspend (accountKey: ByteArray) -> ByteArray?,
    rootSeed: () -> ByteArray,
    compose: (newOwner: ByteArray, x25519: ByteArray, rootSeed: ByteArray) -> ByteArray,
    sendEnvelope: suspend (envelope: ByteArray) -> Unit,
): HandoverSendOutcome {
    val own = ownAccountKey() ?: return HandoverSendOutcome.CANCELLED
    val owner = liveOwner()
    if (owner == null || owner.contentEquals(own)) return HandoverSendOutcome.CANCELLED
    val x25519 = try {
        ownerX25519(owner)
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        null
    } ?: return HandoverSendOutcome.PENDING
    // Re-read after the lookup: the table may have moved while the bundle was
    // fetched.
    val ownerNow = liveOwner()
    if (ownerNow == null || ownerNow.contentEquals(own)) return HandoverSendOutcome.CANCELLED
    if (!ownerNow.contentEquals(owner)) return HandoverSendOutcome.PENDING
    val envelope = try {
        val seed = rootSeed()
        try {
            compose(owner, x25519, seed)
        } finally {
            seed.fill(0)
        }
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        return HandoverSendOutcome.FAILED
    }
    return try {
        sendEnvelope(envelope)
        HandoverSendOutcome.SENT
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        HandoverSendOutcome.FAILED
    }
}

/** The outgoing owner re-sends an owed handover at most this often while
 *  its probe says it is still the root. Chosen, not measured. */
public const val HANDOVER_RESEND_MS: Long = 10L * 60L * 1000L

/** What one step of an owed handover did. */
public enum class OwedHandoverOutcome {
    /** The probe answered 401: the root moved; the owed record goes. */
    DONE,

    /** Sent this round; kept until the probe answers 401. */
    SENT,

    /** Still the root, but sent less than [HANDOVER_RESEND_MS] ago. */
    THROTTLED,

    /** Nothing to hand over (this account is owner again, or the group has no
     *  owner): the owed record goes. */
    CANCELLED,

    /** The probe or the send could not run (offline, 5xx, no box key yet):
     *  retried later. */
    RETRY,
}

/** `true` when an outcome ends the obligation (the owed record is cleared). */
public fun OwedHandoverOutcome.settlesObligation(): Boolean =
    this == OwedHandoverOutcome.DONE || this == OwedHandoverOutcome.CANCELLED

/**
 * Finding 4 — one step of an owed handover: probe with this account's own root
 * ([probeOwnRoot]: `true` on 204, `false` on 401, a throw otherwise); while
 * this account is still the root, (re-)send the envelope ([send], normally
 * [sendRootHandover]) unless it went out less than [HANDOVER_RESEND_MS] ago.
 * Never throws except on cancellation.
 */
public suspend fun runOwedHandover(
    probeOwnRoot: suspend () -> Boolean,
    lastSentAtMs: () -> Long?,
    nowMs: () -> Long,
    send: suspend () -> HandoverSendOutcome,
    log: (String) -> Unit = {},
): OwedHandoverOutcome {
    val stillRoot = try {
        probeOwnRoot()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        log("root probe failed; the handover is retried later")
        return OwedHandoverOutcome.RETRY
    }
    if (!stillRoot) {
        log("root probe answered 401; the handover landed")
        return OwedHandoverOutcome.DONE
    }
    val last = lastSentAtMs()
    if (last != null && nowMs() - last in 0 until HANDOVER_RESEND_MS) return OwedHandoverOutcome.THROTTLED
    return when (send()) {
        HandoverSendOutcome.SENT -> OwedHandoverOutcome.SENT
        HandoverSendOutcome.CANCELLED -> OwedHandoverOutcome.CANCELLED
        HandoverSendOutcome.PENDING, HandoverSendOutcome.FAILED -> OwedHandoverOutcome.RETRY
    }
}

// -----------------------------------------------------------------
// The owed record: which groups this account still owes a handover for
// -----------------------------------------------------------------

/**
 * Where the groups this account still owes a root handover for live, so a
 * handover whose new owner did not resolve (or whose send failed) resumes after
 * a restart. Group ids only: nothing secret. Wiped on forget-device.
 */
public interface OwedHandoverStore {
    public fun groups(accountIdB64: String): Set<String>

    public fun add(accountIdB64: String, groupIdHex: String)

    public fun remove(accountIdB64: String, groupIdHex: String)

    public fun clearForAccount(accountIdB64: String)
}

/** In-memory owed record (tests). */
public class InMemoryOwedHandoverStore : OwedHandoverStore {
    private val rows = ConcurrentHashMap<String, MutableSet<String>>()

    override fun groups(accountIdB64: String): Set<String> = rows[accountIdB64]?.toSet().orEmpty()

    override fun add(accountIdB64: String, groupIdHex: String) {
        rows.getOrPut(accountIdB64) { ConcurrentHashMap.newKeySet() }.add(groupIdHex.lowercase())
    }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        rows[accountIdB64]?.remove(groupIdHex.lowercase())
    }

    override fun clearForAccount(accountIdB64: String) {
        rows.remove(accountIdB64)
    }
}

// -----------------------------------------------------------------
// Receiving (new owner)
// -----------------------------------------------------------------

/** The rank a registrar-seed envelope carries (the core's
 *  `DsSeedEnvelopeRank`, mirrored so the host tests need no native class). */
public enum class SeedEnvelopeRank { MEMBER, ADMIN, ROOT }

/** A registrar-seed envelope's clear header. */
public class SeedEnvelopeHeader(
    public val journalId: ByteArray,
    public val addresseeAccountKey: ByteArray,
    public val rank: SeedEnvelopeRank,
    /** v2 (ADR-0057, amended 2026-10-01): the registrar's public key, public
     *  and readable without opening; the open refuses a seed that does not
     *  derive it. Empty when the caller has none to give. */
    public val registrarPublicKey: ByteArray = ByteArray(0),
)

/** Why a rank-root envelope was dropped, or [ACCEPT]. */
public enum class RootHandoverVerdict {
    ACCEPT,

    /** The envelope names another journal. */
    WRONG_JOURNAL,

    /** Addressed to another account. */
    NOT_ADDRESSED_TO_US,

    /** The live role table does not name this account owner. */
    NOT_OWNER,

    /** This account's key could not be read: fail closed. */
    UNRESOLVED,
}

/**
 * The receive gate for a rank-root envelope. [ownerAccountKey] is the live
 * role table's owner read at receipt. No sender check: the DS refuses a seed
 * that is not the current root.
 */
public fun rootHandoverVerdict(
    ownAccountKey: ByteArray?,
    header: SeedEnvelopeHeader,
    groupId: ByteArray,
    ownerAccountKey: ByteArray?,
): RootHandoverVerdict {
    if (ownAccountKey == null) return RootHandoverVerdict.UNRESOLVED
    if (!header.journalId.contentEquals(groupId)) return RootHandoverVerdict.WRONG_JOURNAL
    if (!header.addresseeAccountKey.contentEquals(ownAccountKey)) return RootHandoverVerdict.NOT_ADDRESSED_TO_US
    if (ownerAccountKey == null || !ownerAccountKey.contentEquals(ownAccountKey)) return RootHandoverVerdict.NOT_OWNER
    return RootHandoverVerdict.ACCEPT
}

/** What [receiveSeedEnvelope] did with one envelope. */
public enum class SeedEnvelopeOutcome {
    /** A registrar seed, kept in the registrar-seed store. */
    REGISTRAR_STORED,

    /** A root handover that passed its gate: the takeover was started. */
    HANDOVER_STARTED,

    /** Dropped (a gate refused it, or it did not open). */
    DROPPED,
}

/**
 * The one dispatch for a received registrar-seed envelope, so the rule that a
 * ROOT-rank envelope never enters the registrar-seed store is structural: the
 * ROOT branch has no path to [storeRegistrarSeed].
 *
 * @param open opens the envelope with the ACCOUNT identity; `null` or a throw
 *   when it does not open. The returned seed is owned here.
 * @param storeRegistrarSeed keeps a member / admin registrar seed; the caller
 *   must copy it (it is wiped on return).
 * @param takeOver starts the new owner's takeover with the opened OLD root
 *   seed, detached; it owns the seed and wipes it.
 * @param onDrop told why an envelope was dropped (for logging; never the seed).
 */
public fun receiveSeedEnvelope(
    header: SeedEnvelopeHeader,
    groupId: ByteArray,
    ownAccountKey: ByteArray?,
    senderAccountKey: ByteArray?,
    ownerAccountKey: ByteArray?,
    open: () -> ByteArray?,
    storeRegistrarSeed: (seed: ByteArray) -> Unit,
    takeOver: (oldRootSeed: ByteArray) -> Unit,
    onDrop: (reason: String) -> Unit = {},
): SeedEnvelopeOutcome {
    if (header.rank == SeedEnvelopeRank.ROOT) {
        val verdict = rootHandoverVerdict(ownAccountKey, header, groupId, ownerAccountKey)
        if (verdict != RootHandoverVerdict.ACCEPT) {
            if (verdict != RootHandoverVerdict.NOT_ADDRESSED_TO_US) onDrop("root handover refused: $verdict")
            return SeedEnvelopeOutcome.DROPPED
        }
        val seed = openOrNull(open) ?: run {
            onDrop("root handover did not open")
            return SeedEnvelopeOutcome.DROPPED
        }
        if (seed.size != REGISTRAR_SEED_LEN) {
            seed.fill(0)
            onDrop("root handover seed has the wrong width")
            return SeedEnvelopeOutcome.DROPPED
        }
        return try {
            takeOver(seed)
            SeedEnvelopeOutcome.HANDOVER_STARTED
        } catch (_: Throwable) {
            seed.fill(0)
            onDrop("root handover could not start")
            SeedEnvelopeOutcome.DROPPED
        }
    }
    val verdict = registrarSeedVerdict(
        ownAccountKey = ownAccountKey,
        addresseeAccountKey = header.addresseeAccountKey,
        senderAccountKey = senderAccountKey,
        ownerAccountKey = ownerAccountKey,
    )
    if (verdict != RegistrarSeedVerdict.ACCEPT) {
        if (verdict != RegistrarSeedVerdict.NOT_ADDRESSED_TO_US) onDrop("registrar seed refused: $verdict")
        return SeedEnvelopeOutcome.DROPPED
    }
    if (!header.journalId.contentEquals(groupId)) {
        onDrop("registrar seed names another journal; refused")
        return SeedEnvelopeOutcome.DROPPED
    }
    val seed = openOrNull(open) ?: run {
        onDrop("registrar seed did not open")
        return SeedEnvelopeOutcome.DROPPED
    }
    return try {
        storeRegistrarSeed(seed)
        SeedEnvelopeOutcome.REGISTRAR_STORED
    } catch (_: Throwable) {
        onDrop("registrar seed not stored")
        SeedEnvelopeOutcome.DROPPED
    } finally {
        seed.fill(0)
    }
}

private fun openOrNull(open: () -> ByteArray?): ByteArray? =
    try {
        open()
    } catch (_: Throwable) {
        null
    }

/** What the new owner's takeover did. */
public enum class TakeOverOutcome {
    /** The root moved to this account's root (204); the registrar set was
     *  reset (or its reset given up on); the reconcile was scheduled. */
    TOOK_OVER,

    /** 401, and this account's own probe says it holds the root: another
     *  device of this account took over already. Marker set; seed dropped. */
    HELD_ELSEWHERE,

    /** 401: the seed is not the current root (the envelope is stale or
     *  forged). Dropped. */
    STALE,

    /** The role table no longer names this account owner. */
    NOT_OWNER,

    /** The root statement kept failing (transiently, or with another 4xx):
     *  the pending seed is KEPT and the takeover retried later. */
    FAILED,
}

/** `true` when the pending seed is no longer needed (every outcome but
 *  [TakeOverOutcome.FAILED]). */
public fun TakeOverOutcome.dropsPendingSeed(): Boolean = this != TakeOverOutcome.FAILED

/** Delays between takeover attempts on a transient failure (network, 5xx):
 *  two retries. Chosen, not measured: a short retry, then one after roughly a
 *  poll interval. */
public val ROOT_HANDOVER_RETRY_DELAYS_MS: List<Long> = listOf(5_000L, 30_000L)

/** The core's bound on one registrar statement's change list
 *  (`lakko_ds_proof::statement::MAX_LIST_ENTRIES`). */
public const val MAX_REGISTRAR_CHANGES: Int = 256

/** A takeover failure worth retrying: anything but an HTTP status below 500
 *  (a network error, a timeout, a 5xx). */
public fun isTransientStatementFailure(error: Throwable): Boolean =
    error !is DsHttpError || error.status >= HTTP_SERVER_ERROR_FLOOR

private const val HTTP_SERVER_ERROR_FLOOR: Int = 500
private const val HTTP_UNAUTHORIZED: Int = 401

private suspend fun withTransientRetry(
    delaysMs: List<Long>,
    sleep: suspend (Long) -> Unit,
    run: suspend () -> Unit,
) {
    var attempt = 0
    while (true) {
        try {
            run()
            return
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (error: Throwable) {
            val wait = delaysMs.getOrNull(attempt)
            if (wait == null || error is NotOwnerAnyMore || !isTransientStatementFailure(error)) throw error
            attempt += 1
            sleep(wait)
        }
    }
}

/**
 * The new owner's takeover with the handed-over OLD root seed: `root` signed by
 * the old root naming this account's derived root; on 204 the "root held"
 * marker is set, then one `registrars-set` signed by the NEW root carries the
 * whole set the live roster calls for (every old registrar not listed goes
 * with it, finding 2), then a fresh appointment reconcile. Does NOT wipe
 * [oldRootSeed]: the caller owns it and keeps it (persisted) while the
 * outcome is [TakeOverOutcome.FAILED]. Never throws except on cancellation.
 *
 * @param ownRootPublicKey `dsJournalRootSigner(ownAccountIdentity, gid).publicKey`.
 * @param stillOwner the live role table still names this account owner;
 *   checked immediately before the `root` PUT.
 * @param oldRootTransfer `dsStatementSigner(oldRootSeed).rootTransfer(gid, newRoot)`.
 * @param newRoot this account's root signer (the NEW root), or `null`.
 * @param desiredRegistrars the registrar set the live roster and role table
 *   call for (lowercase-hex account key -> rank, never this account), or
 *   `null` when unreadable.
 * @param newRegistrarPublicKey the public key of
 *   `dsJournalRegistrarSeed(newRootSeed, accountKey)`.
 * @param probeOwnRoot after a 401: is this account's own root the journal's
 *   root already (another device of this account took over)? A throw reads
 *   as "no".
 * @param markRootHeld set this account's root-held marker for the group.
 * @param put the statement PUT (the `DsClient` helper, which retries a 409
 *   once itself).
 * @param resetAppointments forget which seeds were delivered (every seed
 *   changes with the root) and drop this account's own registrar seed.
 * @param reconcile schedule the normal appointment reconcile under the new root.
 */
public suspend fun takeOverJournal(
    journalId: ByteArray,
    oldRootSeed: ByteArray,
    ownRootPublicKey: () -> ByteArray?,
    stillOwner: suspend () -> Boolean,
    oldRootTransfer: (oldRootSeed: ByteArray, newRoot: ByteArray) -> DsStatement,
    newRoot: () -> DsStatementSigning?,
    desiredRegistrars: suspend () -> Map<String, JournalRank>?,
    newRegistrarPublicKey: (accountKey: ByteArray) -> ByteArray,
    probeOwnRoot: suspend () -> Boolean,
    markRootHeld: () -> Unit,
    put: suspend (make: () -> DsStatement) -> Unit,
    resetAppointments: suspend () -> Unit,
    reconcile: () -> Unit,
    retryDelaysMs: List<Long> = ROOT_HANDOVER_RETRY_DELAYS_MS,
    sleep: suspend (Long) -> Unit = { delay(it) },
    log: (String) -> Unit = {},
): TakeOverOutcome {
    val newRootKey = try {
        ownRootPublicKey()
    } catch (_: Throwable) {
        null
    } ?: return TakeOverOutcome.FAILED

    // 1. The root statement, signed by the OLD root, naming this account's.
    try {
        withTransientRetry(retryDelaysMs, sleep) {
            if (!stillOwner()) throw NotOwnerAnyMore()
            put { oldRootTransfer(oldRootSeed, newRootKey) }
        }
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: NotOwnerAnyMore) {
        return TakeOverOutcome.NOT_OWNER
    } catch (error: Throwable) {
        if (error is DsHttpError && error.status == HTTP_UNAUTHORIZED) {
            val heldAlready = try {
                probeOwnRoot()
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                false
            }
            if (heldAlready) {
                markRootHeld()
                log("root handover already applied by another device of this account")
                reconcile()
                return TakeOverOutcome.HELD_ELSEWHERE
            }
            log("root handover not applied (401): the seed is not the current root; dropped")
            return TakeOverOutcome.STALE
        }
        log("root handover failed; retried later")
        return TakeOverOutcome.FAILED
    }

    // The root is this account's from here: sign with it from now on.
    markRootHeld()

    // 2. One registrars-set under the new root: the whole set the roster
    //    calls for; every old root's registrar not listed is removed with it.
    try {
        val desired = desiredRegistrars()
        val root = newRoot()
        if (root != null && desired != null) {
            withTransientRetry(retryDelaysMs, sleep) {
                sendRegistrarSet(journalId, desired, root, newRegistrarPublicKey, put)
            }
        }
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        // The root moved; the reconcile below sends the set again.
        log("registrar set not reset after root handover")
    }

    // 3. Appoint every member again under the new root.
    try {
        resetAppointments()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        // The reconcile still runs; a stale record costs a missed envelope
        // until the roster next changes.
    }
    reconcile()
    return TakeOverOutcome.TOOK_OVER
}

private class NotOwnerAnyMore : RuntimeException("this account is no longer the owner")
