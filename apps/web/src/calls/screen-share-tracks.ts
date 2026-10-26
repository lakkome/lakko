// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which remote tracks are a shared screen, and how this client names its own
 * (ADR-0083 sections 3 and 4).
 *
 * A share travels as further tracks under a stream id that ends in
 * {@link SCREEN_STREAM_SUFFIX}: the SFU names a publisher's declared share
 * lines `<peer>~screen` for every subscriber; on a one-to-one call the sender
 * names its own share stream that way in the SDP it sends. The suffix is the
 * core's (`screenStreamSuffix()`), repeated here so this module needs no wasm;
 * a test pins the two equal.
 *
 * A screen stream is never a participant, never a camera and never an entry
 * in `video-wants`. Callers ask {@link screenStreamPeerOf} before
 * `splitStreamId` (`light-stream.ts`), which would read `<peer>~screen` as an
 * ordinary main stream of a peer called `<peer>~screen`.
 *
 * Pure: no DOM, no Solid.
 */

/** What a share's tracks travel under (`lakko_calls::screen_share::SCREEN_STREAM_SUFFIX`). */
export const SCREEN_STREAM_SUFFIX = "~screen";

/**
 * The peer a share stream belongs to, or `null` when `streamId` is not a
 * share stream. A bare suffix names no peer and is not a share. On a
 * one-to-one call the part before the suffix is the sender's own random
 * stream id; the caller keys that share by the one-to-one peer id instead.
 */
export function screenStreamPeerOf(streamId: string): string | null {
  if (streamId.length <= SCREEN_STREAM_SUFFIX.length) return null;
  if (!streamId.endsWith(SCREEN_STREAM_SUFFIX)) return null;
  return streamId.slice(0, -SCREEN_STREAM_SUFFIX.length);
}

/**
 * A one-to-one SDP this device sends, with its share stream named as a share.
 *
 * A browser does not let a page choose a `MediaStream`'s id, and the other
 * device reads the stream id off the `a=msid` (and the legacy `a=ssrc ...
 * msid:`) lines of the description it receives. So every occurrence of the
 * share stream's own id in the text that leaves this device is followed by
 * {@link SCREEN_STREAM_SUFFIX}; the local description is left as the browser
 * made it. The id is a random UUID the browser minted, so it occurs nowhere
 * else in an SDP. An occurrence that already carries the suffix is left as
 * it is, so the rewrite is idempotent.
 *
 * `null` or an empty id: the SDP unchanged (no share is being sent).
 */
export function withScreenStreamId(sdp: string, screenStreamId: string | null): string {
  if (screenStreamId === null || screenStreamId.length === 0) return sdp;
  let out = "";
  let from = 0;
  for (;;) {
    const at = sdp.indexOf(screenStreamId, from);
    if (at < 0) break;
    const end = at + screenStreamId.length;
    out += sdp.slice(from, end) + SCREEN_STREAM_SUFFIX;
    // A suffix already there is not written twice.
    from = sdp.startsWith(SCREEN_STREAM_SUFFIX, end) ? end + SCREEN_STREAM_SUFFIX.length : end;
  }
  return out + sdp.slice(from);
}

/** A share's sender could not be sealed, so the share was not sent
 *  (invariant #4). */
export class ScreenShareNotSealedError extends Error {
  constructor(cause?: unknown) {
    super("the screen share could not be sealed; it was not sent");
    this.name = "ScreenShareNotSealedError";
    if (cause !== undefined) (this as { cause?: unknown }).cause = cause;
  }
}
