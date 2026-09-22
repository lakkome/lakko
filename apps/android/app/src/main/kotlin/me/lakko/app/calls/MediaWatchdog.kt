// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * A call with nothing arriving on it is over. The Kotlin twin of
 * `apps/web/src/calls/media-watchdog.ts`.
 *
 * ## The defect this closes
 *
 * A 1x1 call ran for **45 minutes** in the browser after the other side had
 * left it, counting time the whole way (2026-09-21). The transport-level
 * defences did not fire and could not have: the connection state never left
 * `CONNECTED`, because both ends were on one machine and the candidate pair
 * stayed reachable after the peer stopped taking part.
 *
 * That is the gap, and it was wider here than on the web. A peer connection
 * answers "can these two addresses still reach each other", which is not "is
 * there still a call here"; nothing asked the second question. On Android
 * nothing asked the first one either for a CONNECTED call: `DISCONNECTED` was
 * ignored with a comment pointing at an upper-layer watchdog, and that
 * watchdog — the host's establish timer — matches only `Outgoing`,
 * `Connecting` and an accepted `Ringing`.
 *
 * ## The rule
 *
 * Inbound RTP only ever climbs on a call that is happening: a muted microphone
 * still sends RTP, a camera turned off still leaves the audio flowing, and a
 * 1x1 call always carries an audio section both ways. A count that has not
 * moved for the whole window means nothing is arriving, and the call ends the
 * way a dead transport ends it.
 *
 * ## Why NOT the candidate pair
 *
 * The obvious meter is the selected pair's `bytesReceived`, and it is the
 * wrong one: it counts ICE consent checks, which a peer's ICE agent answers
 * every few seconds for as long as its process is alive, call or no call. A
 * watchdog metered on the pair would have watched the 45-minute call's counter
 * climb the whole way and called it healthy — the same blindness as the
 * connection state, wearing a different hat.
 *
 * ## What it does not cover
 *
 * It only runs on a CONNECTED call. A call still being set up has not received
 * anything YET, which is a different thing from having stopped, and is bounded
 * by the host's establish timer instead: a peer who takes a minute to answer
 * is nowhere near this rule.
 */
public object MediaWatchdog {

    /** What the watchdog carries between samples. */
    public data class InboundActivity(
        /** The last inbound RTP packet count, summed across every receiver. */
        val packets: Long,
        /** When that count last CHANGED, on the caller's monotonic clock. */
        val lastChangeAtMs: Long,
    )

    /**
     * Fold one stats sample into the activity.
     *
     * A count that went BACKWARDS counts as activity rather than silence: it
     * means the receivers were replaced (a renegotiation, an ICE restart), and
     * reading a reset as silence would end a call that is working.
     */
    public fun note(prev: InboundActivity?, packets: Long, atMs: Long): InboundActivity =
        if (prev == null || packets != prev.packets) {
            InboundActivity(packets = packets, lastChangeAtMs = atMs)
        } else {
            prev
        }

    /**
     * Whether the call has been silent long enough to be over.
     *
     * A `null` activity is never silent — nothing has been measured yet, and a
     * call that has not produced its first sample has not failed to produce a
     * second. A timeout of zero or less disables the watchdog rather than
     * ending every call instantly, so a caller can turn it off without a
     * second flag.
     */
    public fun hasGoneSilent(
        activity: InboundActivity?,
        atMs: Long,
        timeoutMs: Long,
    ): Boolean {
        if (activity == null || timeoutMs <= 0L) return false
        return atMs - activity.lastChangeAtMs >= timeoutMs
    }
}
