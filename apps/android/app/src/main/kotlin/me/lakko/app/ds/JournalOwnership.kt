// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import uniffi.lakko_ffi_uniffi.DsRegistrarChange
import uniffi.lakko_ffi_uniffi.DsRegistrarEntry
import uniffi.lakko_ffi_uniffi.DsRegistrarRank
import uniffi.lakko_ffi_uniffi.DsSeedEnvelopeRank
import uniffi.lakko_ffi_uniffi.DsStatementSignerInterface

/*
 * ADR-0057 — a journal has an owner; only those who change a group register
 * its epochs.
 *
 * Every group journal has a ROOT key the group's owner derives on every device
 * of its account (`dsJournalRootSigner(accountIdentity, journalId)`). The root
 * claims the journal, appoints REGISTRARS (one per member, ranked `admin` or
 * `member`, the seed sealed to that member inside the group) and may reset,
 * re-rank, revoke and bind the journal's push routing id. An epoch key is
 * registered only by the root or a registrar; a plain member no longer "heals"
 * a missing registration.
 *
 * Every statement is a `PUT /ds/group/<hex>/<segment>` with body
 * `{"statement_b64": ...}` and NO proof header; the core signer builds both.
 *
 * This file is the client half kept free of the native core so every decision
 * runs on the host JVM: [DsStatementSigning] is what the rest of the app signs
 * with (production wraps the core's `DsStatementSigner` in
 * [FfiDsStatementSigning]; a test fakes it), and the pure functions below are
 * the policy.
 */

/** One `PUT` to the Delivery Service, as the core signer built it. */
public data class DsStatement(val path: String, val bodyJson: String)

/** A registrar's rank. The root outranks both. */
public enum class JournalRank { ADMIN, MEMBER }

/** One change of a registrar-management statement: [rank] `null` removes. */
public class JournalRegistrarChange(public val publicKey: ByteArray, public val rank: JournalRank?)

/** One registrar of a whole registrar set (`registrars-set`). */
public class JournalRegistrarEntry(public val publicKey: ByteArray, public val rank: JournalRank)

/** A root or registrar signer (ADR-0057 S1 statements). */
public interface DsStatementSigning {
    /** The 32-byte public key the DS knows this signer by. */
    public val publicKey: ByteArray

    public fun claim(journalId: ByteArray, epoch: ULong, epochKey: ByteArray): DsStatement

    public fun reset(journalId: ByteArray, epoch: ULong, epochKey: ByteArray): DsStatement

    /** `access` in APPEND mode: accepted only at the journal's next epoch
     *  (`409` otherwise). Every commit path registers with this. */
    public fun registerEpoch(
        journalId: ByteArray,
        epoch: ULong,
        epochKey: ByteArray,
        revoke: List<ByteArray>,
    ): DsStatement

    /** `access` in REPLACE mode (second security pass, finding 1): the repair
     *  an owner or admin sends at its own epoch, dropping every lower-rank
     *  registration above it. Never used in a commit path. */
    public fun replaceEpoch(
        journalId: ByteArray,
        epoch: ULong,
        epochKey: ByteArray,
        revoke: List<ByteArray>,
    ): DsStatement

    public fun registrars(journalId: ByteArray, changes: List<JournalRegistrarChange>): DsStatement

    /** `registrars-set` (root only, finding 2): the WHOLE registrar set;
     *  every registrar not listed is removed. */
    public fun registrarsSet(journalId: ByteArray, entries: List<JournalRegistrarEntry>): DsStatement

    public fun pushBinding(journalId: ByteArray, pushGid: ByteArray): DsStatement

    /** `root`: move the journal's root to [newRoot]. Signed by the CURRENT
     *  root (the handed-over seed, ADR-0057 S2 finding 1). */
    public fun rootTransfer(journalId: ByteArray, newRoot: ByteArray): DsStatement
}

/** The core's `DsStatementSigner` as a [DsStatementSigning]. */
public class FfiDsStatementSigning(private val inner: DsStatementSignerInterface) : DsStatementSigning {
    override val publicKey: ByteArray = inner.publicKey()

    override fun claim(journalId: ByteArray, epoch: ULong, epochKey: ByteArray): DsStatement =
        inner.claim(journalId, epoch, epochKey).let { DsStatement(it.path, it.bodyJson) }

    override fun reset(journalId: ByteArray, epoch: ULong, epochKey: ByteArray): DsStatement =
        inner.reset(journalId, epoch, epochKey, null as List<DsRegistrarEntry>?).let { DsStatement(it.path, it.bodyJson) }

    override fun registerEpoch(
        journalId: ByteArray,
        epoch: ULong,
        epochKey: ByteArray,
        revoke: List<ByteArray>,
    ): DsStatement = inner.registerEpoch(journalId, epoch, epochKey, revoke).let { DsStatement(it.path, it.bodyJson) }

    override fun replaceEpoch(
        journalId: ByteArray,
        epoch: ULong,
        epochKey: ByteArray,
        revoke: List<ByteArray>,
    ): DsStatement = inner.replaceEpoch(journalId, epoch, epochKey, revoke).let { DsStatement(it.path, it.bodyJson) }

    override fun registrarsSet(journalId: ByteArray, entries: List<JournalRegistrarEntry>): DsStatement =
        inner.registrarsSet(
            journalId,
            entries.map { DsRegistrarEntry(publicKey = it.publicKey, rank = toFfiRank(it.rank)) },
        ).let { DsStatement(it.path, it.bodyJson) }

    override fun registrars(journalId: ByteArray, changes: List<JournalRegistrarChange>): DsStatement =
        inner.registrars(
            journalId,
            changes.map { change ->
                DsRegistrarChange(
                    publicKey = change.publicKey,
                    rank = change.rank?.let(::toFfiRank),
                )
            },
        ).let { DsStatement(it.path, it.bodyJson) }

    override fun pushBinding(journalId: ByteArray, pushGid: ByteArray): DsStatement =
        inner.pushBinding(journalId, pushGid).let { DsStatement(it.path, it.bodyJson) }

    override fun rootTransfer(journalId: ByteArray, newRoot: ByteArray): DsStatement =
        inner.rootTransfer(journalId, newRoot).let { DsStatement(it.path, it.bodyJson) }

    public companion object {
        /** The journal's root signer for the owner's ACCOUNT identity. */
        public fun root(accountIdentity: uniffi.lakko_ffi_uniffi.Identity, journalId: ByteArray): FfiDsStatementSigning =
            FfiDsStatementSigning(uniffi.lakko_ffi_uniffi.dsJournalRootSigner(accountIdentity, journalId))

        /** The signer of a root or registrar seed. */
        public fun ofSeed(seed: ByteArray): FfiDsStatementSigning =
            FfiDsStatementSigning(uniffi.lakko_ffi_uniffi.dsStatementSigner(seed))

        /** The core rank for a [JournalRank]. */
        public fun toFfiRank(rank: JournalRank): DsRegistrarRank = when (rank) {
            JournalRank.ADMIN -> DsRegistrarRank.ADMIN
            JournalRank.MEMBER -> DsRegistrarRank.MEMBER
        }

        /** The [JournalRank] for a core rank. */
        public fun fromFfiRank(rank: DsRegistrarRank): JournalRank = when (rank) {
            DsRegistrarRank.ADMIN -> JournalRank.ADMIN
            DsRegistrarRank.MEMBER -> JournalRank.MEMBER
        }

        /** The [SeedEnvelopeRank] a decoded registrar-seed envelope carries. */
        public fun fromFfiSeedRank(rank: DsSeedEnvelopeRank): SeedEnvelopeRank = when (rank) {
            DsSeedEnvelopeRank.MEMBER -> SeedEnvelopeRank.MEMBER
            DsSeedEnvelopeRank.ADMIN -> SeedEnvelopeRank.ADMIN
            DsSeedEnvelopeRank.ROOT -> SeedEnvelopeRank.ROOT
        }
    }
}

// -----------------------------------------------------------------
// Who signs what
// -----------------------------------------------------------------

/**
 * Obligation 5 — the statement signer of the epoch a commit reaches: the ROOT
 * when this account owns the journal, else this account's REGISTRAR signer,
 * else `null`, which means this device must not commit (no stage, no direct
 * commit; the sweep is left to an admin or the owner).
 *
 * The two sources are lazy so a non-owner never derives the root, and an owner
 * never reads the registrar store.
 */
public fun commitStatementSigner(
    isOwner: Boolean,
    root: () -> DsStatementSigning?,
    registrar: () -> DsStatementSigning?,
    rootHeld: Boolean = true,
): DsStatementSigning? = when (journalAuthorityKind(isOwner, rootHeld, registrar)) {
    JournalAuthorityKind.ROOT -> root()
    JournalAuthorityKind.REGISTRAR -> registrar()
    JournalAuthorityKind.NONE -> null
}

/** Which key this device signs a journal's statements with. */
public enum class JournalAuthorityKind { ROOT, REGISTRAR, NONE }

/**
 * Finding 4 of the second security pass — owning a group in its role table and
 * holding its journal root on the Delivery Service differ while an ownership
 * transfer is in flight. An owner signs with its root only once the per-(account,
 * group) "root held" marker is set ([rootHeld]); before that it keeps signing
 * with the registrar seed the previous owner appointed it with, and falls back
 * to the root only when it holds no registrar seed (a group this account
 * founded, whose marker was lost or never set on this device). A non-owner
 * signs with its registrar seed or not at all. [registrar] is consulted only
 * when it can matter, and its result is only tested for presence here.
 */
public fun journalAuthorityKind(
    isOwner: Boolean,
    rootHeld: Boolean,
    registrar: () -> Any?,
): JournalAuthorityKind = when {
    isOwner && rootHeld -> JournalAuthorityKind.ROOT
    isOwner -> if (registrar() != null) JournalAuthorityKind.REGISTRAR else JournalAuthorityKind.ROOT
    registrar() != null -> JournalAuthorityKind.REGISTRAR
    else -> JournalAuthorityKind.NONE
}

/**
 * ADR-0057 — register the key of the epoch a commit moves to ([after], the
 * N+1 access signer, whose verifying key is what the server learns) with
 * [statements] (root or registrar), revoking [revoke] in the same step, BEFORE
 * the commit is persisted or published; hand back [before] (N), which signs
 * the commit POST itself.
 *
 * Throws [DsRegistrationUnavailable] (and [put] is never called) when either
 * access signer is missing, the commit does not move the epoch by exactly one,
 * or there is no statement signer — the last is the ordinary state of a plain
 * member, which may not commit at all. The caller aborts on ANY throw.
 */
public suspend fun registerEpochBeforePublish(
    journalId: ByteArray,
    before: DsJournalSigner?,
    after: DsJournalSigner?,
    statements: DsStatementSigning?,
    revoke: List<ByteArray>,
    put: suspend (make: () -> DsStatement) -> Unit,
): DsJournalSigner {
    val current = before
        ?: throw DsRegistrationUnavailable("no access signer for the epoch the commit leaves")
    val next = after
        ?: throw DsRegistrationUnavailable("no access signer for the epoch the commit reaches")
    if (next.epoch != current.epoch + 1uL) {
        throw DsRegistrationUnavailable(
            "the commit moves the epoch from ${current.epoch} to ${next.epoch}, not by one",
        )
    }
    val signer = statements
        ?: throw DsRegistrationUnavailable("this account holds neither the root nor a registrar key")
    val key = next.verifyingKey
    put { signer.registerEpoch(journalId, next.epoch, key, revoke) }
    return current
}

// -----------------------------------------------------------------
// Evictees and their registrar keys
// -----------------------------------------------------------------

/** One roster row as the ownership policy reads it: the leaf and the account
 *  key it resolves to (ADR-0050). */
public class RosterLeaf(public val leafIndex: UInt, public val accountKey: ByteArray)

/**
 * The accounts a commit removes from the group entirely: every account one of
 * whose leaves is in [removedLeaves] and none of whose leaves survives it. A
 * device removal that leaves another device of the same account in the group
 * evicts nobody (the registrar key is per account). Order follows [before];
 * each account at most once.
 */
public fun evictedAccounts(before: List<RosterLeaf>, removedLeaves: Collection<UInt>): List<ByteArray> {
    val removed = removedLeaves.toHashSet()
    val byAccount = LinkedHashMap<String, MutableList<RosterLeaf>>()
    for (leaf in before) byAccount.getOrPut(hexOf(leaf.accountKey)) { mutableListOf() }.add(leaf)
    return byAccount.values
        .filter { leaves -> leaves.any { it.leafIndex in removed } && leaves.all { it.leafIndex in removed } }
        .map { it.first().accountKey }
}

/** The accounts in [before] and in none of [after] — the evictees of an
 *  already-merged commit. */
public fun evictedBetween(before: List<ByteArray>, after: List<ByteArray>): List<ByteArray> {
    val kept = after.map(::hexOf).toHashSet()
    val seen = HashSet<String>()
    return before.filter { key -> val hex = hexOf(key); hex !in kept && seen.add(hex) }
}

/**
 * The revoke list of a commit: the registrar public key of each evictee, which
 * only the ROOT can derive (`dsJournalRegistrarSeed(rootSeed, accountKey)`).
 * An admin committer passes `null` for [deriveRegistrarKey] and sends an empty
 * list; the owner revokes on its next reconcile (the ADR residual). A key the
 * derivation fails for is skipped, never guessed.
 */
public fun revokeListFor(
    evictees: List<ByteArray>,
    ownAccountKey: ByteArray?,
    deriveRegistrarKey: ((accountKey: ByteArray) -> ByteArray)?,
): List<ByteArray> {
    val derive = deriveRegistrarKey ?: return emptyList()
    return evictees
        .filterNot { ownAccountKey != null && it.contentEquals(ownAccountKey) }
        .mapNotNull { key -> runCatching { derive(key) }.getOrNull() }
}

// -----------------------------------------------------------------
// Appointments (obligation 3)
// -----------------------------------------------------------------

/**
 * The registrar set the owner wants: every member account other than its own,
 * ranked `admin` when the role table names it an admin, else `member`. Keyed
 * by lowercase hex of the account key.
 */
public fun desiredRegistrars(
    memberAccounts: List<ByteArray>,
    ownerAccountKey: ByteArray,
    admins: List<ByteArray>,
): Map<String, JournalRank> {
    val adminHex = admins.map(::hexOf).toHashSet()
    val ownerHex = hexOf(ownerAccountKey)
    val out = LinkedHashMap<String, JournalRank>()
    for (key in memberAccounts) {
        val hex = hexOf(key)
        if (hex == ownerHex) continue
        out[hex] = if (hex in adminHex) JournalRank.ADMIN else JournalRank.MEMBER
    }
    return out
}

/** The most entries one `registrars-set` statement may carry
 *  (`lakko_ds_proof::statement::MAX_REGISTRARS`). */
public const val MAX_REGISTRAR_SET: Int = 4096

/** A deterministic fingerprint of a desired registrar set (sorted `hex:rank`
 *  pairs): what the session remembers it last sent, so an unchanged roster
 *  sends nothing. */
public fun registrarSetFingerprint(desired: Map<String, JournalRank>): String =
    desired.entries
        .sortedBy { it.key }
        .joinToString(",") { (hex, rank) -> hex + ":" + if (rank == JournalRank.ADMIN) "A" else "M" }

/**
 * Finding 2 of the second security pass — sign and send `registrars-set` for
 * [desired] with [root]: each entry is the REGISTRAR public key the root
 * derives for that account (never the account key), sorted by account key so
 * the statement is deterministic. Throws the DS refusal (or a derivation
 * failure); nothing is recorded by the caller then.
 */
public suspend fun sendRegistrarSet(
    journalId: ByteArray,
    desired: Map<String, JournalRank>,
    root: DsStatementSigning,
    registrarPublicKeyFor: (accountKey: ByteArray) -> ByteArray,
    put: suspend (make: () -> DsStatement) -> Unit,
) {
    if (desired.size > MAX_REGISTRAR_SET) {
        throw IllegalStateException("the registrar set exceeds the server bound")
    }
    val entries = desired.entries.sortedBy { it.key }.map { (hex, rank) ->
        val accountKey = hexToBytesOrNull(hex) ?: throw IllegalStateException("malformed account key")
        JournalRegistrarEntry(registrarPublicKeyFor(accountKey), rank)
    }
    put { root.registrarsSet(journalId, entries) }
}

/** What [reconcileAppointments] did. */
public data class AppointmentResult(
    /** Accounts that hold their seed envelope now, at which rank: store it as
     *  the new record. */
    val delivered: Map<String, JournalRank>,
    /** The fingerprint of the set this pass sent, or `null` when no
     *  `registrars-set` went out (unchanged, or refused). */
    val sentSet: String?,
    /** `false` when some wanted member's envelope is left for next time (its
     *  X25519 key did not resolve, or its send failed), or the set was
     *  refused. */
    val complete: Boolean,
)

/**
 * Obligation 3, as amended by finding 2 of the second security pass: CONVERGE,
 * do not diff. The owner sends `registrars-set` with the WHOLE [desired] set
 * whenever it differs from the one this session last sent for the group
 * ([lastSentSet], `null` for never); the DS drops every registrar not listed,
 * so a removal missed by any local record cannot leave an evicted member's key
 * registered. Then each account whose [delivered] rank differs from the
 * desired one is sent its seed, sealed to its account X25519 key inside the
 * group ([sendEnvelope]). The local record only tracks envelopes.
 *
 * An account whose X25519 key does not resolve yet is registered all the same
 * (registering a key hands nothing to anyone lacking the seed) and its
 * envelope is left for the next pass. A refused `registrars-set` sends no
 * envelope and records nothing.
 *
 * @param registrarSeedFor the root's derivation for one member account
 *   (`dsJournalRegistrarSeed(rootSeed, accountKey)`).
 * @param registrarPublicKeyOf the public key of a registrar seed.
 */
public suspend fun reconcileAppointments(
    journalId: ByteArray,
    desired: Map<String, JournalRank>,
    delivered: Map<String, JournalRank>,
    lastSentSet: String?,
    root: DsStatementSigning,
    registrarSeedFor: (accountKey: ByteArray) -> ByteArray,
    registrarPublicKeyOf: (seed: ByteArray) -> ByteArray,
    memberX25519: suspend (accountKey: ByteArray) -> ByteArray?,
    put: suspend (make: () -> DsStatement) -> Unit,
    sendEnvelope: suspend (accountKey: ByteArray, x25519: ByteArray, rank: JournalRank, seed: ByteArray) -> Unit,
): AppointmentResult {
    val fingerprint = registrarSetFingerprint(desired)
    val setDue = lastSentSet != fingerprint
    val owed = desired.filter { (hex, rank) -> delivered[hex] != rank }
    val kept = delivered.filter { (hex, rank) -> desired[hex] == rank }
    if (!setDue && owed.isEmpty() && kept.size == delivered.size) {
        return AppointmentResult(delivered, sentSet = null, complete = true)
    }
    if (setDue) {
        try {
            sendRegistrarSet(
                journalId = journalId,
                desired = desired,
                root = root,
                registrarPublicKeyFor = { accountKey ->
                    val seed = registrarSeedFor(accountKey)
                    try {
                        registrarPublicKeyOf(seed)
                    } finally {
                        seed.fill(0)
                    }
                },
                put = put,
            )
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            return AppointmentResult(delivered, sentSet = null, complete = false)
        }
    }
    val next = LinkedHashMap(kept)
    var complete = true
    for ((hex, rank) in owed) {
        val accountKey = hexToBytesOrNull(hex) ?: continue
        val x25519 = try {
            memberX25519(accountKey)
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            null
        }
        if (x25519 == null) {
            complete = false
            continue
        }
        val seed = registrarSeedFor(accountKey)
        try {
            sendEnvelope(accountKey, x25519, rank, seed)
            next[hex] = rank
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            complete = false
        } finally {
            seed.fill(0)
        }
    }
    return AppointmentResult(next, sentSet = if (setDue) fingerprint else null, complete = complete)
}

private fun hexToBytesOrNull(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    return try {
        ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
    }
}

// -----------------------------------------------------------------
// Receiving a registrar seed (obligation 4)
// -----------------------------------------------------------------

/** Why a received registrar-seed envelope was kept or dropped. */
public enum class RegistrarSeedVerdict {
    /** Addressed to this account, sent by the group's owner: open and store. */
    ACCEPT,

    /** Addressed to another account: not ours to open. */
    NOT_ADDRESSED_TO_US,

    /** The MLS sender did not resolve to the owner of the live role table. */
    SENDER_NOT_OWNER,

    /** The sender or the role table could not be read: fail closed. */
    UNRESOLVED,
}

/**
 * The receive gate. [senderAccountKey] is the MLS sender resolved AT RECEIPT
 * (C-68); [ownerAccountKey] is the live role table's owner. Both must be known
 * and equal, and the envelope must name this account.
 */
public fun registrarSeedVerdict(
    ownAccountKey: ByteArray?,
    addresseeAccountKey: ByteArray,
    senderAccountKey: ByteArray?,
    ownerAccountKey: ByteArray?,
): RegistrarSeedVerdict {
    if (ownAccountKey == null) return RegistrarSeedVerdict.UNRESOLVED
    if (!addresseeAccountKey.contentEquals(ownAccountKey)) return RegistrarSeedVerdict.NOT_ADDRESSED_TO_US
    if (senderAccountKey == null || ownerAccountKey == null) return RegistrarSeedVerdict.UNRESOLVED
    if (!senderAccountKey.contentEquals(ownerAccountKey)) return RegistrarSeedVerdict.SENDER_NOT_OWNER
    return RegistrarSeedVerdict.ACCEPT
}

// -----------------------------------------------------------------
// Claims
// -----------------------------------------------------------------

/** What a claim attempt did. */
public enum class JournalClaimOutcome {
    /** 204: first claim, or the identical claim re-sent. */
    CLAIMED,

    /** 409 on a group this device founded at its founding epoch: re-founded
     *  under the same id (lost-state recovery); the root reset re-claimed it. */
    RESET,

    /** 409: the journal is claimed with another chain (this root at a later
     *  epoch, or another root). Left alone: never reset a live chain. */
    ALREADY_CLAIMED,
}

/**
 * Obligations 1, 2 and 7 — claim [journalId] at [epoch] with the root
 * [statements]; on a `409`, reset the chain ONLY when [refounded] (this device
 * founded the group under a deterministic id and still sits at the founding
 * epoch, so the chain on the server belongs to a group that is gone). Every
 * other refusal propagates.
 */
public suspend fun claimOrReset(
    journalId: ByteArray,
    epoch: ULong,
    epochKey: ByteArray,
    statements: DsStatementSigning,
    refounded: Boolean,
    put: suspend (make: () -> DsStatement) -> Unit,
): JournalClaimOutcome {
    try {
        put { statements.claim(journalId, epoch, epochKey) }
        return JournalClaimOutcome.CLAIMED
    } catch (e: DsHttpError) {
        if (e.status != HTTP_CONFLICT_STATUS) throw e
    }
    if (!refounded) return JournalClaimOutcome.ALREADY_CLAIMED
    put { statements.reset(journalId, epoch, epochKey) }
    return JournalClaimOutcome.RESET
}

/** Finding 3 of the second security pass — a claim at creation did not land
 *  (the journal carries another chain): nothing may be published for it. */
public class JournalClaimRefused(message: String) : IllegalStateException(message)

/**
 * Finding 3 — the claim at CREATION fails closed: a conversation group, a
 * DeviceGroup founding or a channel's PublisherGroup is published (a Welcome
 * sent, a channel announced) only after this returns. [claim] is normally
 * [claimOrReset]; its refusals propagate, and a `409` that was not a reset
 * ([JournalClaimOutcome.ALREADY_CLAIMED]) throws [JournalClaimRefused].
 */
public suspend fun requireClaim(claim: suspend () -> JournalClaimOutcome): JournalClaimOutcome {
    val outcome = claim()
    if (outcome == JournalClaimOutcome.ALREADY_CLAIMED) {
        throw JournalClaimRefused("the journal is already claimed under another chain")
    }
    return outcome
}

/**
 * Finding 3 for a founding that is persisted before it is claimed (the
 * DeviceGroup bootstrap): while the founding is [unconfirmed], claim it
 * ([requireClaim]) and only then [clear] the marker; a failure propagates with
 * the marker kept, so the caller publishes nothing and the next attempt claims
 * again. A confirmed founding claims nothing here.
 */
public suspend fun claimFoundingIfUnconfirmed(
    unconfirmed: Boolean,
    claim: suspend () -> JournalClaimOutcome,
    clear: () -> Unit,
) {
    if (!unconfirmed) return
    requireClaim(claim)
    clear()
}

/**
 * Finding 4 — "am I still the root?": a `root` statement naming [root]'s own
 * key, signed by it. `true` on 204 (this signer is the journal's root), `false`
 * on the uniform 401 (it is not); anything else throws.
 */
public suspend fun probeRoot(
    journalId: ByteArray,
    root: DsStatementSigning,
    put: suspend (make: () -> DsStatement) -> Unit,
): Boolean =
    try {
        put { root.rootTransfer(journalId, root.publicKey) }
        true
    } catch (e: DsHttpError) {
        if (e.status == HTTP_UNAUTHORIZED_STATUS) false else throw e
    }

/** What a push-binding attempt did (obligation 8): log-and-continue states. */
public enum class PushBindingOutcome {
    BOUND,
    CONFLICT,
    DISABLED,
    FAILED,

    /** This device holds neither the root nor an admin registrar key: nothing
     *  was sent (a member never binds). */
    NOT_PERMITTED,
}

/** Classify a push-binding refusal: `409` bound elsewhere, `503` disabled. */
public fun pushBindingOutcomeOf(error: Throwable?): PushBindingOutcome = when {
    error == null -> PushBindingOutcome.BOUND
    error is DsHttpError && error.status == HTTP_CONFLICT_STATUS -> PushBindingOutcome.CONFLICT
    error is DsHttpError && error.status == HTTP_UNAVAILABLE_STATUS -> PushBindingOutcome.DISABLED
    else -> PushBindingOutcome.FAILED
}

private const val HTTP_CONFLICT_STATUS: Int = 409
private const val HTTP_UNAUTHORIZED_STATUS: Int = 401
private const val HTTP_UNAVAILABLE_STATUS: Int = 503

/** Lowercase hex, the key spelling of every map here. */
public fun hexOf(bytes: ByteArray): String = DsClient.pseudonymHex(bytes)
