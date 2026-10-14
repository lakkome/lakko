// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The light companion and the middle track of a remote camera.
 *
 * A publisher sends its camera as two ordinary senders, a main one and a
 * light one (ADR-0070 step 1), and the SFU gives each subscriber two video
 * tracks: the **main** one, whose MSID stream id is the source peer id (as
 * for every remote stream), and a **light companion** that carries the light
 * sender's picture, whose MSID stream id is the source peer id followed by
 * {@link LIGHT_STREAM_SUFFIX}. A publisher without a light sender has no
 * companion.
 *
 * From three people in the call on, a publisher also sends a **middle**
 * sender (ADR-0070 step 5), which arrives under the peer id followed by
 * {@link MIDDLE_STREAM_SUFFIX}; the SFU forwards its packets only while this
 * device asks for that peer's middle picture (`video-wants.ts`).
 *
 * The companion is a second picture of a person already in the call, and the
 * middle track a third. Neither is ever a participant of its own: no tile, no
 * audio sink, no camera flag. The tile of its peer decodes the companion
 * beside the picture it watches and shows it while that one stands still
 * (`layer-display.ts`); the middle track is one of the two pictures a tile
 * can watch (`video-wants.ts`).
 *
 * Pure: no DOM, no Solid. The two call adapters (`sfu-call-ui.ts`,
 * `multi-joiner-call.ts`) keep their companion streams with the helpers here,
 * so the rule is written once.
 */

/** What the SFU appends to a peer id to name that peer's light companion. */
export const LIGHT_STREAM_SUFFIX = "~light";

/** What the SFU appends to a peer id to name that peer's middle track
 *  (ADR-0070 step 5). */
export const MIDDLE_STREAM_SUFFIX = "~mid";

/** Which of a peer's streams a remote stream id names. */
export type RemoteStreamRole = "main" | "middle" | "light";

/** A remote MSID stream id, read. */
export interface RemoteStreamId {
  /** The source peer. */
  readonly peerId: string;
  /** The peer's main stream (audio and the camera as it is sent), its middle
   *  track, or its light companion. */
  readonly role: RemoteStreamRole;
}

const SUFFIXES: ReadonlyArray<readonly [string, RemoteStreamRole]> = [
  [LIGHT_STREAM_SUFFIX, "light"],
  [MIDDLE_STREAM_SUFFIX, "middle"],
];

/**
 * Split a remote stream id into the peer it belongs to and which of that
 * peer's streams it is. One suffix is read, at the end. A bare suffix (no
 * peer id before it) names no peer and is read as an ordinary stream id.
 */
export function splitStreamId(streamId: string): RemoteStreamId {
  for (const [suffix, role] of SUFFIXES) {
    if (streamId.length > suffix.length && streamId.endsWith(suffix)) {
      return { peerId: streamId.slice(0, -suffix.length), role };
    }
  }
  return { peerId: streamId, role: "main" };
}

/** The light stream id of a peer (the inverse of {@link splitStreamId}). */
export function lightStreamIdOf(peerId: string): string {
  return `${peerId}${LIGHT_STREAM_SUFFIX}`;
}

/** The middle stream id of a peer (the inverse of {@link splitStreamId}). */
export function middleStreamIdOf(peerId: string): string {
  return `${peerId}${MIDDLE_STREAM_SUFFIX}`;
}

/** The peer -> light stream map both call adapters keep. The peer -> middle
 *  stream map is the same shape and is kept with the same two helpers. */
export type LightStreams<S> = ReadonlyMap<string, S>;

/** `streams` with `peerId`'s companion set. */
export function withLightStream<S>(streams: LightStreams<S>, peerId: string, stream: S): LightStreams<S> {
  if (streams.get(peerId) === stream) return streams;
  const next = new Map(streams);
  next.set(peerId, stream);
  return next;
}

/**
 * `streams` without `peerId`'s companion. With `stream` given, only that very
 * stream is removed: a companion that was replaced by a newer one is not
 * dropped by the older track's late `ended`.
 */
export function withoutLightStream<S>(streams: LightStreams<S>, peerId: string, stream?: S): LightStreams<S> {
  if (!streams.has(peerId)) return streams;
  if (stream !== undefined && streams.get(peerId) !== stream) return streams;
  const next = new Map(streams);
  next.delete(peerId);
  return next;
}
