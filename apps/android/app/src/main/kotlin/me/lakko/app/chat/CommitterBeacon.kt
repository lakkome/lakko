// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences

/**
 * ADR-0045 S4b — the committer liveness beacon: emit it, observe it, and let the
 * lowest-ranked *active* candidate take the sweep.
 *
 * ## ADR-0050 section 2.5a item 6: devices of one account share a rank
 *
 * Every leaf of a ranked account holds the account's rank, so two devices of
 * one person beacon at the SAME rank. The core's fold breaks that tie by
 * signature key: a device defers to a fresh observation of a strictly lower
 * rank, or of the same rank with a bytewise lower key, and ignores
 * observations of its own key. The acting device of an account is therefore
 * its lowest-keyed ACTIVE one (a lost device that stays in the roster stops
 * beaconing and stops being deferred to). That is why each projected
 * observation carries its sender's key and the fold is handed this device's
 * own leaf key.
 *
 * Android mirror of `apps/web/src/chat/committer-beacon.ts`.
 *
 * ## What this closes
 *
 * S4a made admins propose and the **owner** sweep. Rank 0 acts without
 * consulting anything, so an admin-only group — an owner who has not opened the
 * app since the request landed — simply waits, with no deadline. That is the
 * designed behaviour, not a bug, but it is also the whole of the waiting: every
 * candidate is willing and none of them knows it is allowed.
 *
 * This slice supplies the missing fact. A candidate announces itself on the
 * group journal; every other candidate hears it, ranks its sender, and stands
 * down only for someone **strictly** better placed. Hearing nothing means
 * *act* — silence is the answer that nobody better placed is running, not a
 * failure to elect.
 *
 * Android is the platform where the takeover should actually work: the chat
 * screen runs a deliberate 30 s DS long poll, which is the
 * `COMMITTER_BEACON_WORST_CASE_POLL_MS` term the core's freshness window is
 * constructed against, so a beacon emitted here is observed inside the window it
 * was sized for.
 *
 * ## The rules live in the core; this module supplies what the core cannot see
 *
 * `shouldEmitBeacon` and `actingCommitterVerdict` are pure, total and
 * clock-free, and both are consumed over the FFI rather than reimplemented here
 * — three clients each hand-rolling one rule is exactly the drift class that
 * produced ADR-0044's F-A replay. What this module owns is the three things the
 * core deliberately cannot supply:
 *
 * 1. **WHO** — the beacon envelope is field-less, so the sender comes from the
 *    MLS-authenticated sender leaf, resolved through `members()` to a signature
 *    key and ranked by `Group.committerRank`.
 * 2. **HOW OLD** — from the DS row's server-assigned `created_at`, which every
 *    member reads identically. The payload carries no timestamp precisely so a
 *    candidate cannot assert its own freshness: that would be a liveness claim
 *    no peer can check, i.e. a duty nobody could take away.
 * 3. **FOREGROUNDEDNESS** — a caller obligation the core cannot check. See
 *    [beaconEmitVerdict].
 *
 * ## Rank at evaluation time, never at receipt
 *
 * A rank is a **position** in the role table's canonical order (`0` = owner,
 * `k + 1` = the k-th admin), not an identity. Promoting, demoting or removing
 * any admin renumbers every admin sorting after it, so a rank captured when a
 * beacon arrived and compared later against this device's own rank — which is
 * necessarily current — is not merely stale: it is a position in a **different
 * ordering**, and the comparison silently stops meaning anything.
 *
 * So [recordBeaconObservation] persists the sender's 32-byte signature **key**
 * and the row timestamp, and [projectObservations] ranks the key at the moment
 * the verdict is taken. The key rather than the leaf index for the second half
 * of the same reason: OpenMLS reuses a vacated leaf for the next member added,
 * so a persisted leaf later resolves to somebody else.
 *
 * ## This does NOT close the race
 *
 * Each candidate folds over its own poll-timed view, and a beacon is stale by up
 * to one interval even when its sender is perfectly healthy, so two candidates
 * can both reach `Act`. ADR-0045's stand-down journal re-read and its
 * staged-commit rollback both remain required; [runCommitterSweep] still runs
 * them unchanged, and reading this verdict as a licence to delete either would
 * be a mis-reading. A heartbeat removes the *wait*, not the *race*.
 */

/**
 * The FFI's `ActingCommitter`, restated as a client-side sealed class.
 *
 * Declared here rather than reused from `uniffi.lakko_ffi_uniffi` so the sweep
 * orchestration and this module stay drivable from a plain JVM host test — the
 * same reason `ChatClient.toEvent` takes its codecs as parameters. The adapter
 * in [ChatRepository] converts the FFI value into this one, and that conversion
 * is the ONLY place the two shapes meet.
 *
 * [Defer] and [NotACandidate] stay distinct rather than collapsing to one
 * `false`, because one has something to re-evaluate on the next poll and the
 * other never will.
 */
internal sealed class ActingCommitter {
    object Act : ActingCommitter()

    /**
     * @param toRank the rank of the best-placed fresh observation (equal to
     *   this device's own when it defers to a lower-keyed device of its own
     *   account) — informational only.
     * @param toSignatureKey that observation's 32-byte signature key, for a log
     *   line; `null` when the defer is this client's own fail-closed answer
     *   (nothing was observed, the verdict was unreadable).
     */
    data class Defer(val toRank: UInt?, val toSignatureKey: ByteArray? = null) : ActingCommitter() {
        override fun equals(other: Any?): Boolean =
            other is Defer && other.toRank == toRank &&
                (other.toSignatureKey?.contentEquals(toSignatureKey) ?: (toSignatureKey == null))

        override fun hashCode(): Int = 31 * (toRank?.hashCode() ?: 0) + (toSignatureKey?.contentHashCode() ?: 0)
    }

    object NotACandidate : ActingCommitter()
}

/** Why this device did or did not emit a beacon on this tick. */
internal enum class BeaconEmitVerdict {
    /** Emit — this device is a candidate, foregrounded, and past its throttle. */
    EMIT,

    /** No committer rank: not an owner or admin, and (ADR-0050 section 2.5a)
     *  not a member holding a device admission it could sweep. A beacon from
     *  it could never make it the acting committer (every receiver's fold
     *  drops it), so emitting one is pure journal litter and a pure activity
     *  leak. A plain member WITH such an admission has a rank (every device of
     *  its account does) and beacons for as long as it holds one. */
    NOT_A_CANDIDATE,

    /**
     * ADR-0050 section 2.5a item 8 — this device holds neither the journal root
     * nor a registrar seed for the group, so it could not register the epoch a
     * sweep reaches and never stages one. A beacon from it would make every
     * other candidate (a sibling device of its own account included) defer to
     * a device that cannot complete a sweep. The same check the sweep makes
     * before staging.
     */
    CANNOT_REGISTER,

    /** The app is not foregrounded. See [beaconEmitVerdict]. */
    NOT_FOREGROUND,

    /**
     * ADR-0045 UX §6.2.8 item 2 — this device's own membership has ENDED, so it
     * can never sweep again. Distinct from [NOT_A_CANDIDATE] because it is
     * PERMANENT rather than a function of the current role table, and reported
     * separately so a log line says which of the two it was.
     */
    MEMBERSHIP_ENDED,

    /** A beacon went out less than one interval ago. */
    THROTTLED,
}

/**
 * One projected observation, in the shape the FFI's `BeaconObservation`
 * constructor takes. Both fields are the OBSERVER's own re-derivation — nothing
 * in the beacon payload contributes to either.
 */
internal class ProjectedObservation(
    /** The rank re-derived for this beacon's sender AT VERDICT TIME, or `null`
     *  when that sender is not a candidate (a demoted admin with no rank, a
     *  member with nothing to sweep, or a key that no longer resolves). Since
     *  ADR-0050 section 2.5a every leaf of a ranked account holds the
     *  account's rank, a plain member's included while it holds a device
     *  admission. */
    val rank: UInt?,
    /** `now - created_at` on the DS row, in milliseconds. Never negative. */
    val ageMs: ULong,
    /** The sender's 32-byte leaf signature key, resolved at receipt — the
     *  tie-break between devices of one account (section 2.5a item 6). */
    val signatureKey: ByteArray,
)

/**
 * How often this client's emitter WAKES.
 *
 * **Not the beacon interval** — that is the core's
 * `committerBeaconIntervalMs`, enforced by `shouldEmitBeacon`, and is never
 * hand-copied. This only bounds how long after the chat screen resumes the first
 * beacon waits, so it has to be comfortably inside the interval and is otherwise
 * free to be coarse: each tick derives this device's own rank, which costs an
 * Ed25519 key derivation.
 */
internal const val BEACON_TICK_MS: Long = 15_000L

/**
 * The send-side gate, with the one rule the core cannot check bolted on.
 *
 * **The foreground gate is not cosmetic.** A beacon means *able to sweep*, and
 * on this client a sweep runs only at the tail of a poll driven by a RESUMED
 * chat screen. The ADR-0041 / C-59 background decrypt
 * ([me.lakko.app.push.PushBackgroundDecrypt]) is read-only-throwaway and
 * structurally cannot commit, and a backgrounded process can be frozen or killed
 * at any moment — so a beacon emitted from the background would be a lie that
 * costs the group a takeover: every other candidate would defer to a device that
 * is not going to sweep, and would keep doing so for as long as the backgrounded
 * app kept beaconing.
 *
 * The gate runs FIRST and the throttle decides everything else, so the candidate
 * rule and the interval rule stay the core's — the [BeaconEmitVerdict.NOT_A_CANDIDATE]
 * / [BeaconEmitVerdict.THROTTLED] split below is only a label for the log line,
 * derived after the core has already said no.
 *
 * @param ownRank this device's ADR-0045 committer rank. `0` is the OWNER, not
 *   "no rank" — a `null` check and a `> 0` check are different questions.
 * @param throttle the FFI `shouldEmitBeacon`. Injected so the rule is the
 *   core's, and so a host test can drive this function without the `.so`.
 * @param membershipEnded ADR-0045 UX §6.2.8 item 2 — this device's own
 *   membership has ended. Checked FIRST, before the foreground gate; see the
 *   body.
 * @param canRegister ADR-0050 section 2.5a item 8 — this device holds the
 *   journal root or a registrar seed for the group (the sweep's own pre-stage
 *   check). `false` emits nothing ([BeaconEmitVerdict.CANNOT_REGISTER]).
 */
internal fun beaconEmitVerdict(
    ownRank: UInt?,
    foreground: Boolean,
    lastEmittedAtMs: Long?,
    nowMs: Long,
    throttle: (UInt?, ULong?, ULong) -> Boolean,
    membershipEnded: Boolean = false,
    canRegister: Boolean = true,
): BeaconEmitVerdict {
    // ADR-0045 UX §6.2.8 item 2 — AHEAD of the foreground gate, because it is
    // the permanent condition of the two and because the role table a `Remove`
    // leaves behind still names an evicted admin an admin: `ownRank` would come
    // back non-null and the throttle would happily say EMIT. The emit itself is
    // an ordinary MLS application message, which `createApplicationMessage`
    // refuses for an inactive group, so what this prevents is not a wrong
    // beacon reaching peers but a throwing emit re-attempted on every tick,
    // forever, for a group that will never accept one.
    if (membershipEnded) return BeaconEmitVerdict.MEMBERSHIP_ENDED
    if (!foreground) return BeaconEmitVerdict.NOT_FOREGROUND
    // ADR-0050 section 2.5a item 8 — ahead of the throttle: a device that
    // cannot register an epoch would only make better-keyed siblings defer to
    // it, and it never stages. Only a ranked device gets here meaningfully; a
    // device with no rank reports NOT_A_CANDIDATE, which is the stronger
    // statement.
    if (!canRegister && ownRank != null) return BeaconEmitVerdict.CANNOT_REGISTER
    val allowed = throttle(
        ownRank,
        lastEmittedAtMs?.takeIf { it >= 0L }?.toULong(),
        nowMs.coerceAtLeast(0L).toULong(),
    )
    if (allowed) return BeaconEmitVerdict.EMIT
    return if (ownRank == null) BeaconEmitVerdict.NOT_A_CANDIDATE else BeaconEmitVerdict.THROTTLED
}

/** What [projectObservations] produced: the fold's input, plus the keys worth keeping. */
internal data class ProjectedBeacons(
    val projected: List<ProjectedObservation>,
    val liveKeys: List<String>,
)

/**
 * Project the observation store into the list the fold consumes, ranking every
 * sender AGAINST THE CURRENT ROLE TABLE.
 *
 * Returns the live keys alongside, so the caller can prune in the same pass: an
 * entry whose sender no longer ranks is a member who was demoted, removed, or
 * whose key no longer resolves, and it will never contribute again.
 *
 * A row that appears to come from the FUTURE is clamped to age `0` rather than
 * wrapping. That is the safe direction as the FFI states it — an age that reads
 * too fresh makes this device defer (it waits), while one that reads too stale
 * makes it act (it may double-sweep). `ageMs` is a `ULong` on the ABI, and a
 * negative one would not merely be wrong: unsigned, it would wrap to a value
 * *above* every freshness window forever, silently converting "just arrived"
 * into "ancient".
 */
internal fun projectObservations(
    observations: Map<String, Long>,
    rankOf: (String) -> UInt?,
    nowMs: Long,
    decodeKey: (String) -> ByteArray? = ::decodeBeaconKey,
): ProjectedBeacons {
    val projected = mutableListOf<ProjectedObservation>()
    val liveKeys = mutableListOf<String>()
    for ((senderKeyB64, atMs) in observations) {
        // A stored key that is not a 32-byte signature key can name nobody,
        // and the core refuses the WHOLE fold over one such observation, so it
        // is dropped (and pruned) rather than passed on.
        val key = decodeKey(senderKeyB64)?.takeIf { it.size == BEACON_SIGNATURE_KEY_LEN } ?: continue
        // RULE 1: ranked HERE, at verdict time, never at receipt.
        val rank = rankOf(senderKeyB64) ?: continue
        liveKeys.add(senderKeyB64)
        val age = nowMs - atMs
        projected.add(
            ProjectedObservation(
                rank = rank,
                ageMs = (if (age < 0L) 0L else age).toULong(),
                signatureKey = key,
            ),
        )
    }
    return ProjectedBeacons(projected, liveKeys)
}

/** A leaf signature key's width; the core's fold refuses any other. */
internal const val BEACON_SIGNATURE_KEY_LEN: Int = 32

/** The store's key spelling (standard base64, [outboxB64]) back to bytes, or
 *  `null` for a malformed entry. */
internal fun decodeBeaconKey(senderKeyB64: String): ByteArray? =
    try {
        java.util.Base64.getDecoder().decode(senderKeyB64)
    } catch (_: IllegalArgumentException) {
        null
    }

// ---------------------------------------------------------------------------
// The observation store
// ---------------------------------------------------------------------------

/**
 * Where a group's beacon observations live: `senderSignatureKeyB64 -> DS row ms`.
 *
 * Deliberately NOT encrypted, unlike
 * [me.lakko.app.push.PushRoutingSecretStore]: an observation is a signature key
 * that is already public inside the group plus a server-assigned timestamp, so
 * there is no key material here. It is the same posture as the web reference's
 * `localStorage` and as [me.lakko.app.push.PushRouteStore].
 *
 * Behind an interface so the pure decision below is drivable from a host test
 * with no `Context`.
 */
internal interface BeaconObservationStore {
    fun load(accountIdB64: String, groupIdHex: String): Map<String, Long>

    fun save(accountIdB64: String, groupIdHex: String, observations: Map<String, Long>)

    fun clearForAccount(accountIdB64: String)
}

/**
 * Record that a beacon landed.
 *
 * **The NEWEST timestamp wins** — the opposite of [recordPendingRequestObservation],
 * where the FIRST observation of a request wins so a re-broadcast cannot reset
 * its age. Here recency IS the signal: the whole question is *how long since this
 * candidate last proved it was running*, so an older stamp must never overwrite a
 * newer one (a re-fetched journal batch would otherwise age a live candidate out
 * and hand its duty away).
 *
 * A row with no parseable timestamp never reaches here — the caller drops it
 * rather than storing it. There is no age to measure, so it is no evidence of
 * liveness; storing it as "fresh" would defer this device forever with nothing
 * to age out, and storing it as "stale" is what dropping it already achieves.
 *
 * The store is naturally bounded by the candidate count (one entry per signature
 * key, and [projectObservations] prunes anything that no longer ranks), so it
 * needs no age-based GC of its own.
 */
internal fun recordBeaconObservation(
    store: BeaconObservationStore,
    accountIdB64: String,
    groupIdHex: String,
    senderKeyB64: String,
    atMs: Long,
) {
    val observations = store.load(accountIdB64, groupIdHex)
    val existing = observations[senderKeyB64]
    if (existing != null && existing >= atMs) return
    store.save(accountIdB64, groupIdHex, observations + (senderKeyB64 to atMs))
}

/**
 * The whole receive-side decision, composed: read the store, rank every sender
 * against the CURRENT role table, prune what no longer ranks, and hand the
 * projection to the core's fold.
 *
 * @param ownSignatureKey this device's own 32-byte leaf signature key (the
 *   key the group runs on), so the core can break a same-rank tie between
 *   devices of one account and ignore this device's own echoed beacon.
 * @param fold the FFI `actingCommitterVerdict` behind a thin adapter, so the
 *   rule stays the core's and this function stays host-testable.
 */
internal fun decideActingCommitter(
    store: BeaconObservationStore,
    accountIdB64: String,
    groupIdHex: String,
    ownRank: UInt?,
    ownSignatureKey: ByteArray,
    rankOf: (String) -> UInt?,
    nowMs: Long,
    freshnessWindowMs: ULong,
    fold: (UInt?, ByteArray, List<ProjectedObservation>, ULong) -> ActingCommitter,
    decodeKey: (String) -> ByteArray? = ::decodeBeaconKey,
): ActingCommitter {
    val observations = store.load(accountIdB64, groupIdHex)
    val (projected, liveKeys) = projectObservations(observations, rankOf, nowMs, decodeKey)
    if (liveKeys.size != observations.size) {
        store.save(accountIdB64, groupIdHex, observations.filterKeys { it in liveKeys.toSet() })
    }
    // This device's own echoed beacon stays in the projection: the core
    // ignores an observation of the observer's own key, so the client does
    // not restate that rule.
    return fold(ownRank, ownSignatureKey, projected, freshnessWindowMs)
}

/**
 * The production [BeaconObservationStore] — one plain `SharedPreferences` file,
 * one row per `(account, group)` holding a `key=timestamp` list.
 *
 * Row key: `"committer-beacons-v1:" + accountIdB64 + NUL + groupIdHex`, where NUL
 * is written as the Kotlin escape `\u0000` — a RAW NUL in the source would make
 * git classify this file as binary and render no diff (ADR-0042 MEDIUM-3, and
 * the same convention [me.lakko.app.push.PushRoutingSecretStore] follows).
 * `accountIdB64` is standard base64 and `groupIdHex` is lowercase hex, so the
 * single-NUL separator is unambiguous and [clearForAccount] can match every
 * group of an account by key prefix.
 *
 * Row value: `senderKeyB64 + ':' + <decimal ms>` entries joined by `','`. Base64
 * contains no `':'` or `','`, so both separators are unambiguous.
 *
 * Never throws: a corrupt or absent row reads as empty, which degrades to "heard
 * nothing" — and hearing nothing means *act*. That is the correct direction for a
 * store failure, because the sweep's stand-down and rollback already handle two
 * candidates acting, whereas a store failure that made this device defer would
 * make it wait on evidence it does not have.
 */
internal class SharedPrefsBeaconObservationStore(context: Context) : BeaconObservationStore {

    private val appContext: Context = context.applicationContext

    private fun prefs(): SharedPreferences =
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun load(accountIdB64: String, groupIdHex: String): Map<String, Long> = try {
        decodeRow(prefs().getString(rowKey(accountIdB64, groupIdHex), null))
    } catch (_: Throwable) {
        emptyMap()
    }

    override fun save(
        accountIdB64: String,
        groupIdHex: String,
        observations: Map<String, Long>,
    ) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                val key = rowKey(accountIdB64, groupIdHex)
                val editor = prefs().edit()
                if (observations.isEmpty()) editor.remove(key) else editor.putString(key, encodeRow(observations))
                editor.apply()
            } catch (_: Throwable) {
                // Best-effort — losing the store costs a takeover's promptness,
                // never its correctness.
            }
    
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        try {
            val prefix = KEY_PREFIX + accountIdB64 + ROW_SEPARATOR
            val p = prefs()
            val victims = p.all.keys.filter { it.startsWith(prefix) }
            if (victims.isEmpty()) return
            val editor = p.edit()
            for (k in victims) editor.remove(k)
            editor.apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    internal companion object {
        private const val PREFS_NAME = "lakko-committer-beacons"
        private const val KEY_PREFIX = "committer-beacons-v1:"

        /** The `0x00` byte, as a Kotlin escape — see the class doc. */
        private const val ROW_SEPARATOR = "\u0000"

        private fun rowKey(accountIdB64: String, groupIdHex: String): String =
            KEY_PREFIX + accountIdB64 + ROW_SEPARATOR + groupIdHex

        /** Package-visible so the codec round-trip is host-testable without a `Context`. */
        internal fun encodeRow(observations: Map<String, Long>): String =
            observations.entries.joinToString(",") { "${it.key}:${it.value}" }

        internal fun decodeRow(raw: String?): Map<String, Long> {
            if (raw.isNullOrEmpty()) return emptyMap()
            val out = mutableMapOf<String, Long>()
            for (part in raw.split(',')) {
                val sep = part.lastIndexOf(':')
                if (sep <= 0) continue
                val at = part.substring(sep + 1).toLongOrNull() ?: continue
                out[part.substring(0, sep)] = at
            }
            return out
        }
    }
}
