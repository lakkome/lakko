// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A call with nothing arriving on it is over.
 *
 * ## The defect this closes
 *
 * A 1x1 call ran for **45 minutes** in the browser after the other side had
 * left it, counting time the whole way and writing `Incoming call, 45:12` into
 * the conversation when it was finally ended by hand (2026-09-21). The
 * transport-level defences did not fire and could not have: `failed` ends a
 * call at once and `disconnected` ends it after
 * {@link CallSessionTiming.iceDisconnectedGraceMs}, but the ICE state never
 * left `connected` — the two ends were on one machine, so the candidate pair
 * stayed reachable even after the peer stopped taking part.
 *
 * That is the gap. A peer connection answers "can these two addresses still
 * reach each other", which is not the same question as "is there still a call
 * here". Nothing in the session asked the second question, so nothing could
 * answer it, whichever end was at fault.
 *
 * ## The rule
 *
 * Inbound RTP only ever climbs on a call that is happening: a muted microphone
 * still sends RTP, a camera turned off still leaves the audio flowing, and a
 * 1x1 call always has an audio section in both directions. So a count that has
 * not moved for {@link CallSessionTiming.mediaSilenceTimeoutMs} means nothing
 * is arriving, and the call ends as `error` — the same ending a dead transport
 * gets.
 *
 * ## Why NOT the candidate pair
 *
 * The obvious meter is the selected pair's `bytesReceived`, and it is the
 * wrong one: it counts ICE consent checks, which a peer's ICE agent answers
 * every few seconds for as long as its process is alive, call or no call. A
 * watchdog metered on the pair would have watched the 45-minute call's counter
 * climb the whole way and called it healthy — the same blindness as the ICE
 * state itself, wearing a different hat. `inbound-rtp.packetsReceived` moves
 * only when media actually arrives.
 *
 * ## What it does not cover
 *
 * Deliberately NOT a replacement for the ICE handling: this is slower on
 * purpose, and it exists for the case the ICE state cannot see. And it only
 * runs on a CONNECTED call — a call still being set up has not received
 * anything YET, which is a different thing from having stopped, and is bounded
 * by {@link CallSessionTiming.connectTimeoutMs} instead. A peer who takes a
 * minute to answer is nowhere near this rule.
 */

/** What the watchdog carries between samples. */
export interface InboundActivity {
  /** The last inbound RTP packet count, summed across every receiver. */
  readonly packets: number;
  /** When that count last CHANGED, on the caller's monotonic clock. */
  readonly lastChangeAtMs: number;
}

/**
 * Fold one stats sample into the activity.
 *
 * A count that went BACKWARDS counts as activity rather than silence: it means
 * the receivers were replaced (a renegotiation, an ICE restart), and treating
 * a reset as thirty seconds of silence would end a call that is working.
 */
export function noteInboundMedia(
  prev: InboundActivity | null,
  packets: number,
  atMs: number,
): InboundActivity {
  if (prev === null || packets !== prev.packets) return { packets, lastChangeAtMs: atMs };
  return prev;
}

/**
 * Whether the call has been silent long enough to be over.
 *
 * `null` activity is never silent — nothing has been measured yet, and a call
 * that has not produced its first sample has not failed to produce a second.
 * A timeout of `0` or less disables the watchdog rather than ending every
 * call instantly, so a caller can turn it off without a second flag.
 */
export function inboundHasGoneSilent(
  activity: InboundActivity | null,
  atMs: number,
  silenceTimeoutMs: number,
): boolean {
  if (activity === null) return false;
  if (silenceTimeoutMs <= 0) return false;
  return atMs - activity.lastChangeAtMs >= silenceTimeoutMs;
}
