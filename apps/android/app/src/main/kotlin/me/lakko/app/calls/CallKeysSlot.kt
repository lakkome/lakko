// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The SFrame overlap window of a mid-call rekey, in ms (G-18, ADR-0029):
 * how long the previous epoch's key still opens frames. The group call's
 * rekey and an arm that has to rekey first ([CallKeysSlot]) use the same
 * value. Mirrors the web `SFRAME_OVERLAP_MS` and the Rust core
 * `OVERLAP_DEFAULT_MS`.
 */
internal const val SFRAME_REKEY_OVERLAP_MS: Long = 1_000L

/**
 * The one set of SFrame call keys a peer connection holds, and the seed
 * they are at (G-18, ADR-0029).
 *
 * Every encryptor and decryptor of a call is built on the same native
 * `LakkoCallKeys`, so a mid-call rekey is one call ([rekey]) that every
 * transform follows. This class holds the rule that keeps a transform from
 * ever being built on keys at a seed other than the one asked for:
 *
 * - The first [build] creates the keys from its seed.
 * - [build] with the seed the keys are at uses them.
 * - [build] with a NEW seed first rekeys the keys to it (with [defaultOverlapMs],
 *   or the overlap of the last [rekey]), then uses them; a rekey that fails
 *   builds nothing, and the caller arms nothing (security invariant #4).
 * - [build] with a seed the keys were at BEFORE (a superseded one, e.g. an
 *   arm that read the seed just before a concurrent rekey) uses the current
 *   keys and does not rekey back: rolling a live call back to an older epoch
 *   would make every encryptor seal under it. The ring still holds the
 *   previous key during the overlap, so nothing a peer sent is lost.
 *
 * Pure over its four operations so the rule is host-testable; the peer
 * connection passes the `me.lakko.sframe.LakkoCallKeys` calls. Every method
 * is synchronized: arms run on libwebrtc's threads and the caller's, a rekey
 * on the caller's.
 *
 * @param K the keys type (the native wrapper in production).
 */
internal class CallKeysSlot<K : Any>(
    private val create: (seed: ByteArray) -> K?,
    private val rekeyKeys: (keys: K, seed: ByteArray, nowMs: Long, overlapMs: Long) -> Boolean,
    private val pruneKeys: (keys: K, nowMs: Long) -> Unit,
    private val releaseKeys: (keys: K) -> Unit,
    private val nowMs: () -> Long,
    defaultOverlapMs: Long,
) {
    private val lock = Any()
    private var keys: K? = null
    private var seed: ByteArray? = null
    private val superseded = ArrayDeque<ByteArray>()
    private var overlapMs: Long = defaultOverlapMs
    private var closed = false

    /**
     * Run [block] on the call keys at [seed], creating or rekeying them as
     * described on the class; `null` when they cannot be (the caller must
     * arm nothing) or [block] returns `null`.
     */
    fun <R : Any> build(seed: ByteArray, block: (keys: K) -> R?): R? = synchronized(lock) {
        val keys = keysAt(seed) ?: return@synchronized null
        block(keys)
    }

    private fun keysAt(seed: ByteArray): K? {
        if (closed) return null
        val current = keys
        if (current == null) {
            val created = create(seed) ?: return null
            keys = created
            this.seed = seed.copyOf()
            return created
        }
        if (this.seed?.contentEquals(seed) == true) return current
        if (superseded.any { it.contentEquals(seed) }) return current
        if (!rekeyKeys(current, seed, nowMs(), overlapMs)) return null
        moveTo(seed)
        return current
    }

    /**
     * Rotate the keys to [seed] (one native rekey for the whole call).
     * `true` when nothing needed to change (no keys yet, or already at
     * [seed]) or the rekey succeeded; `false` when it failed, in which case
     * the keys stay at their current seed.
     */
    fun rekey(seed: ByteArray, nowMs: Long, overlapMs: Long): Boolean = synchronized(lock) {
        this.overlapMs = overlapMs
        val current = keys ?: return@synchronized true
        if (this.seed?.contentEquals(seed) == true) return@synchronized true
        if (!rekeyKeys(current, seed, nowMs, overlapMs)) return@synchronized false
        moveTo(seed)
        true
    }

    /** Drop the retained previous key once it has expired at [nowMs]. */
    fun prune(nowMs: Long) {
        synchronized(lock) { keys?.let { pruneKeys(it, nowMs) } }
    }

    /**
     * Release the keys and forget every seed. Call after the transforms
     * built on them are released. Idempotent.
     */
    fun clear() {
        synchronized(lock) {
            closed = true
            keys?.let(releaseKeys)
            keys = null
            seed?.fill(0)
            seed = null
            superseded.forEach { it.fill(0) }
            superseded.clear()
        }
    }

    /** The keys are held (for tests and logs). */
    val isHeld: Boolean
        get() = synchronized(lock) { keys != null }

    private fun moveTo(next: ByteArray) {
        seed?.let { superseded.addLast(it) }
        while (superseded.size > MAX_SUPERSEDED) superseded.removeFirst().fill(0)
        seed = next.copyOf()
    }

    companion object {
        /** How many earlier seeds are remembered so a stale arm is not
         *  taken for a new epoch. A call moves one epoch per membership
         *  commit; an arm that read a seed more than this many epochs old
         *  is not expected. */
        const val MAX_SUPERSEDED: Int = 16
    }
}
