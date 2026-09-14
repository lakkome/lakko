// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/** One remote ICE candidate as it arrived on the signaling wire. */
public data class RemoteIceCandidate(
    val sdpMid: String,
    val sdpMLineIndex: Int,
    val candidateSdp: String,
)

/**
 * Holds remote ICE candidates that arrive before the 1x1 peer connection can
 * take them, and releases them in arrival order once it can.
 *
 * ## The defect it closes
 *
 * The caller trickles its candidates right behind its offer. On the answering
 * side the offer only rings: the `PeerConnection` is built when the user
 * accepts, so every candidate that arrived while ringing reached a session
 * with no connection and was dropped. On both sides a candidate can also land
 * between the SDP arriving and `setRemoteDescription` completing (it runs in a
 * coroutine); libwebrtc's synchronous `addIceCandidate` refuses a candidate
 * while there is no remote description. Losing the candidates that carry the
 * only reachable path (a relay candidate, say) leaves ICE with nothing to
 * connect over.
 *
 * ## Contract
 *
 *   * Until [markRemoteDescriptionApplied], [offer] keeps the candidate and
 *     returns `true`; the caller must not apply it.
 *   * [markRemoteDescriptionApplied] returns everything kept, oldest first,
 *     exactly once; from then on [offer] returns `false` and the caller
 *     applies the candidate directly.
 *   * [reset] forgets everything and re-arms buffering (call teardown).
 *   * At most [capacity] candidates are kept; later ones are dropped, so a
 *     peer cannot grow this without bound while a call rings.
 *
 * Thread-safe: every method takes the instance lock.
 */
public class RemoteCandidateBuffer(private val capacity: Int = DEFAULT_CAPACITY) {

    private val lock = Any()
    private val pending = ArrayList<RemoteIceCandidate>()
    private var remoteDescriptionApplied = false
    private var droppedOverCapacity = 0

    /**
     * Keep [candidate] if the remote description is not applied yet.
     *
     * @return `true` when the buffer took (or, over capacity, discarded) the
     *   candidate; `false` when the caller should apply it now.
     */
    public fun offer(candidate: RemoteIceCandidate): Boolean = synchronized(lock) {
        if (remoteDescriptionApplied) return false
        if (pending.size < capacity) {
            pending.add(candidate)
        } else {
            droppedOverCapacity += 1
        }
        true
    }

    /** Record that the remote description is set; returns the kept candidates, oldest first. */
    public fun markRemoteDescriptionApplied(): List<RemoteIceCandidate> = synchronized(lock) {
        if (remoteDescriptionApplied) return emptyList()
        remoteDescriptionApplied = true
        val drained = pending.toList()
        pending.clear()
        drained
    }

    /** Forget every kept candidate and buffer again (a new call on this session object). */
    public fun reset(): Unit = synchronized(lock) {
        pending.clear()
        remoteDescriptionApplied = false
        droppedOverCapacity = 0
    }

    /** Candidates currently kept. */
    public val size: Int
        get() = synchronized(lock) { pending.size }

    /** Candidates discarded because the buffer was full. */
    public val dropped: Int
        get() = synchronized(lock) { droppedOverCapacity }

    public companion object {
        /**
         * A 1x1 call gathers a handful of candidates per network interface
         * (host, server-reflexive, relay). Chosen, not measured: well above
         * that, small enough that a hostile peer cannot hold much memory.
         */
        public const val DEFAULT_CAPACITY: Int = 64
    }
}
