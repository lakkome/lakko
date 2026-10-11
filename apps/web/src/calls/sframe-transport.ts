// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which browser interface carries encoded media frames to the SFrame worker.
 *
 * The seal and the open are the same code whatever the browser
 * (`./sframe-worker-core.ts`); what differs between engines is only how a
 * sender's or a receiver's stream of encoded frames is handed to a worker.
 * There are two standard-shaped ways, and this module is the ONE place that
 * decides between them:
 *
 *   * `"script-transform"` - the W3C Encoded Transform API. The main thread
 *     sets `sender.transform = new RTCRtpScriptTransform(worker, options)`
 *     and the worker receives an `rtctransform` event with the readable /
 *     writable pair. Preferred wherever it exists.
 *   * `"encoded-streams"` - the earlier insertable-streams interface of
 *     Chromium. The peer connection is constructed with
 *     `{ encodedInsertableStreams: true }`, the main thread calls
 *     `sender.createEncodedStreams()` / `receiver.createEncodedStreams()` and
 *     transfers the readable / writable pair to the worker in a message.
 *     Used where `RTCRtpScriptTransform` is absent (seen on Edge 133,
 *     Chromium 133: a guest could not join a link call there).
 *   * `null` - neither. No call that needs frame encryption is opened
 *     (security invariant #4: never a call without it, never a pass-through).
 *
 * The choice is made from what the page exposes, never from a browser name or
 * version. It has two consequences that must agree, which is why both are
 * read from here: the peer connection's configuration
 * ({@link sframePeerConnectionConfig}) and the way a transform is attached
 * (`./sframe-transform.ts`).
 *
 * On the `"encoded-streams"` path a sender or receiver whose streams were
 * never taken does not pass media at all: the browser holds its frames for a
 * reader that never comes. That is a stall, not plaintext, so a missed attach
 * fails closed; `coverEncodedStreams` in `./sframe-transform.ts` is what keeps
 * one from being missed.
 *
 * `encodedInsertableStreams` is set ONLY on the `"encoded-streams"` path. On
 * a browser that takes the script-transform path it is left out. An engine
 * that does not know the member ignores it (a dictionary member it has no
 * definition for). What an engine that knows BOTH interfaces does when
 * `sender.transform` is set on a connection built with the member was not
 * checked in any browser; leaving the member out there is the choice that
 * does not depend on the answer, and it keeps one interface per connection.
 */

/** How encoded frames reach the SFrame worker in this browser. */
export type SFrameTransport = "script-transform" | "encoded-streams";

/** The part of the page's globals the detection reads. A test passes its own;
 *  left out, `globalThis`. */
export interface SFrameTransportScope {
  readonly RTCRtpScriptTransform?: unknown;
  readonly RTCRtpSender?: unknown;
  readonly RTCRtpReceiver?: unknown;
}

function hasCreateEncodedStreams(ctor: unknown): boolean {
  if (typeof ctor !== "function") return false;
  const proto = (ctor as { prototype?: unknown }).prototype;
  if (typeof proto !== "object" || proto === null) return false;
  return typeof (proto as { createEncodedStreams?: unknown }).createEncodedStreams === "function";
}

/**
 * The transport this browser offers, or `null` when it offers none.
 * `RTCRtpScriptTransform` is preferred; the insertable-streams interface is
 * taken only when both the sender and the receiver have it (a call needs to
 * seal and to open).
 */
export function sframeTransport(
  scope: SFrameTransportScope = globalThis as SFrameTransportScope,
): SFrameTransport | null {
  if (typeof scope.RTCRtpScriptTransform === "function") return "script-transform";
  if (hasCreateEncodedStreams(scope.RTCRtpSender) && hasCreateEncodedStreams(scope.RTCRtpReceiver)) {
    return "encoded-streams";
  }
  return null;
}

/**
 * The configuration to construct an SFrame-sealed call's `RTCPeerConnection`
 * with: `base` unchanged on the script-transform path (and when there is no
 * transport, where no connection is built at all), and `base` plus
 * `encodedInsertableStreams: true` on the insertable-streams path. Every
 * peer connection whose senders and receivers get an SFrame transform MUST be
 * built through this, or `createEncodedStreams()` throws on it.
 */
export function sframePeerConnectionConfig(
  base: RTCConfiguration,
  transport: SFrameTransport | null = sframeTransport(),
): RTCConfiguration {
  if (transport !== "encoded-streams") return base;
  return { ...base, encodedInsertableStreams: true } as RTCConfiguration;
}

/** The one line a call says at its start about its frame encryption path. */
export function sframeTransportLine(transport: SFrameTransport | null = sframeTransport()): string {
  return `sframe transport: ${transport ?? "none"}`;
}
