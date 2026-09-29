// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.util.concurrent.ConcurrentHashMap
import uniffi.lakko_ffi_uniffi.DsProofSignerInterface

/*
 * ADR-0053 — a claimed journal on the Delivery Service answers only a request
 * signed with a key it was claimed under.
 *
 * This file is the client half of that rule, kept free of the native core so
 * every decision in it runs on the host JVM:
 *
 *   * [DsJournalSigner] is what [DsClient] signs with. Production wraps the
 *     core's `DsProofSigner` ([FfiDsJournalSigner]); a test fakes it.
 *   * [DsSignerRegistry] maps a journal to the signer of its CURRENT epoch and
 *     keeps the one before it, which is what the 401 retry of the client
 *     contract (point 5) signs with. The chat repository installs a source per
 *     conversation group, the DeviceGroup sync installs one for the DeviceGroup
 *     journal, and the session installs the two mailbox signers — so the many
 *     `DsClient()` instances the app builds all sign without any call site
 *     having to thread a signer through.
 *   * [dsClockSkewed] is the one skew rule: a refused request whose response
 *     `Date` is more than a minute from the device clock is reported as a
 *     wrong clock rather than a delivery failure.
 */

/** The request header a proof travels in (`lakko_ds_proof::PROOF_HEADER_NAME`,
 *  also `dsProofHeaderName()` on the FFI; spelled here so the host tests do
 *  not need the native library). */
public const val DS_PROOF_HEADER: String = "x-lakko-ds-proof"

/** Copy the user reads when a signed request is refused and the device clock
 *  is the likely cause (ADR-0053 client contract point 5). */
public const val DS_CLOCK_SKEW_COPY: String = "Check this device's date and time."

/** How far the response `Date` may be from the device clock before a refusal
 *  is blamed on the clock. */
public const val DS_CLOCK_SKEW_TOLERANCE_MS: Long = 60_000L

/** One journal's proof signer at one epoch. */
public interface DsJournalSigner {
    /** The epoch the signer proves membership at (`dsMailboxEpoch()` for a
     *  mailbox). */
    public val epoch: ULong

    /** The `x-lakko-ds-proof` value for one request: the SERVICE path, the
     *  raw query without `?`, the exact body bytes. */
    public fun sign(method: String, path: String, query: String, body: ByteArray): String

    /** The 32-byte verifying key the Delivery Service learns for this epoch
     *  (ADR-0057: what a root or registrar statement registers). */
    public val verifyingKey: ByteArray

    /** The JSON body of `PUT /ds/mailbox/<hex>/access`. */
    public fun mailboxClaimBody(): String
}

/** The core's `DsProofSigner` as a [DsJournalSigner]. */
public class FfiDsJournalSigner(private val inner: DsProofSignerInterface) : DsJournalSigner {
    override val epoch: ULong = inner.epoch()

    override fun sign(method: String, path: String, query: String, body: ByteArray): String =
        inner.sign(method, path, query, body)

    override val verifyingKey: ByteArray by lazy { inner.verifyingKey() }

    override fun mailboxClaimBody(): String = inner.mailboxClaimBodyJson()
}

/**
 * Where a journal's current signer comes from. [epoch] is read on every
 * lookup and must be cheap; [derive] runs only when the epoch moved.
 * Either may throw or return `null` (the journal's handle is gone), which
 * reads as "no signer" and sends the request unsigned.
 *
 * A handle with no cheap epoch read (the FFI `DeviceGroup` has none) sets
 * [epochReadable] to `false`: every lookup then derives, and the epoch of
 * the derived signer decides whether the cache moves.
 */
public interface DsSignerSource {
    public fun epoch(): ULong?

    public fun derive(): DsJournalSigner?

    public val epochReadable: Boolean get() = true

    /**
     * ADR-0057 obligation 2 — the lazy claim, present only where this
     * account OWNS the journal: claim it with the root at the current epoch
     * and answer whether the claim landed. [DsClient] calls it once on a
     * signed request's `401`, then retries. `null` (every non-owner) means a
     * `401` is surfaced: a plain member never claims and never self-registers.
     */
    public val ownerClaim: (suspend () -> Boolean)? get() = null

    /**
     * ADR-0057, the second security pass (finding 1) — told when a signed
     * request of this journal was refused (401) at [epoch], this device's
     * current epoch, after every retry [DsClient] makes and not because of the
     * device clock. The chat repository notes the group for the repair check
     * (an owner or admin replaces a fake next epoch); `null` for every other
     * journal. Must not block and must not throw.
     */
    public val onRefusedAtHead: ((epoch: ULong) -> Unit)? get() = null
}

/** A source that never changes — a mailbox signer. */
public class StaticDsSignerSource(private val signer: DsJournalSigner) : DsSignerSource {
    override fun epoch(): ULong = signer.epoch

    override fun derive(): DsJournalSigner = signer
}

/** Registry key of a group journal (conversation group, DeviceGroup,
 *  PublisherGroup). */
public fun dsGroupKey(groupId: ByteArray): String = "g/" + DsClient.pseudonymHex(groupId)

/** Registry key of a group journal from its lowercase hex id. */
public fun dsGroupKeyHex(groupIdHex: String): String = "g/" + groupIdHex.lowercase()

/** Registry key of a mailbox. */
public fun dsMailboxKey(pseudonym: ByteArray): String = "m/" + DsClient.pseudonymHex(pseudonym)

/**
 * The journal -> signer map every [DsClient] consults.
 *
 * Per entry it caches the signer of the epoch it last saw and the one before
 * it. A lookup re-reads the source's epoch; when it moved forward the cached
 * signer becomes the previous one and a fresh one is derived. When it moved
 * BACKWARD (an ADR-0040 abort reset the group to its durable epoch) the stale
 * cache is dropped and nothing newer than the current epoch is kept.
 */
public class DsSignerRegistry {
    private class Entry(@Volatile var source: DsSignerSource, @Volatile var owner: String) {
        var current: DsJournalSigner? = null
        var previous: DsJournalSigner? = null
    }

    private val entries = ConcurrentHashMap<String, Entry>()

    /** Install (or replace) the source of [key]. Cached signers survive a
     *  replacement, so a record rebuilt over the same group keeps its
     *  previous-epoch signer. [owner] lets a subsystem clear only its own. */
    public fun install(key: String, source: DsSignerSource, owner: String = "") {
        entries.compute(key) { _, old ->
            if (old == null) {
                Entry(source, owner)
            } else {
                old.source = source
                old.owner = owner
                old
            }
        }
    }

    /** Install a fixed signer (a mailbox). */
    public fun installStatic(key: String, signer: DsJournalSigner, owner: String = "") {
        install(key, StaticDsSignerSource(signer), owner)
    }

    public fun remove(key: String) {
        entries.remove(key)
    }

    /** Drop every entry [owner] installed. */
    public fun clearOwner(owner: String) {
        entries.entries.removeIf { it.value.owner == owner }
    }

    public fun clear() {
        entries.clear()
    }

    public fun has(key: String): Boolean = entries.containsKey(key)

    /** The signer of [key]'s current epoch, or `null` when the journal is not
     *  known here (the request then goes out unsigned). */
    public fun current(key: String): DsJournalSigner? {
        val entry = entries[key] ?: return null
        synchronized(entry) {
            refresh(entry)
            return entry.current
        }
    }

    /** ADR-0057 — the owner's lazy-claim hook of [key]'s source, or `null`
     *  when this account does not own the journal (or it is unknown here). */
    public fun ownerClaim(key: String): (suspend () -> Boolean)? = entries[key]?.source?.ownerClaim

    /** Finding 1 — report a refusal at [epoch] to [key]'s source, if it
     *  listens. Never throws. */
    public fun refusedAtHead(key: String, epoch: ULong) {
        try {
            entries[key]?.source?.onRefusedAtHead?.invoke(epoch)
        } catch (_: Throwable) {
            // A listener that fails holds nothing up.
        }
    }

    /** The signer of the epoch before the current one, when this device saw
     *  it. Refreshed first, so it is never the current epoch. */
    public fun previous(key: String): DsJournalSigner? {
        val entry = entries[key] ?: return null
        synchronized(entry) {
            refresh(entry)
            return entry.previous
        }
    }

    private fun refresh(entry: Entry) {
        val cached = entry.current
        if (entry.source.epochReadable) {
            val epoch = try {
                entry.source.epoch()
            } catch (_: Throwable) {
                null
            } ?: return
            if (cached != null && cached.epoch == epoch) return
        }
        val fresh = try {
            entry.source.derive()
        } catch (_: Throwable) {
            null
        } ?: return
        if (cached != null && cached.epoch == fresh.epoch) return
        if (cached != null) {
            if (fresh.epoch > cached.epoch) {
                entry.previous = cached
            } else {
                // Rolled back: nothing at or above the current epoch may stand
                // in as "previous".
                val prev = entry.previous
                if (prev != null && prev.epoch >= fresh.epoch) entry.previous = null
            }
        }
        entry.current = fresh
    }

    public companion object {
        /** The process-wide registry [DsClient] consults by default. */
        public val shared: DsSignerRegistry = DsSignerRegistry()
    }
}

/**
 * Whether a refusal carrying [dateHeader] is best explained by the device
 * clock: the header parses as an RFC 1123 date and is more than
 * [DS_CLOCK_SKEW_TOLERANCE_MS] from [nowMs]. An absent or unparseable header
 * is not evidence of skew.
 */
public fun dsClockSkewed(dateHeader: String?, nowMs: Long): Boolean {
    if (dateHeader.isNullOrBlank()) return false
    val serverMs = try {
        ZonedDateTime.parse(dateHeader.trim(), DateTimeFormatter.RFC_1123_DATE_TIME)
            .toInstant()
            .toEpochMilli()
    } catch (_: Throwable) {
        return false
    }
    return kotlin.math.abs(serverMs - nowMs) > DS_CLOCK_SKEW_TOLERANCE_MS
}

/** ADR-0053 — a commit cannot be registered with the Delivery Service, so it
 *  must not be published. */
public class DsRegistrationUnavailable(message: String) : IllegalStateException(message)
