// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The light companion of a remote camera.
 *
 * For a publisher that sends two simulcast layers the SFU gives each
 * subscriber two video tracks: the **main** one, whose MSID stream id is the
 * source peer id (as for every remote stream), and a **light companion** that
 * carries only the light layer, whose MSID stream id is the source peer id
 * followed by {@link LIGHT_STREAM_SUFFIX}. A publisher without simulcast has
 * no companion.
 *
 * The companion is a second picture of a person already in the call. It is
 * never a participant of its own: no tile, no audio sink, no camera flag. The
 * tile of its peer decodes it beside the main track and shows it while the
 * main one stands still (`layer-display.ts`).
 *
 * Pure: no DOM, no Solid. The two call adapters (`sfu-call-ui.ts`,
 * `multi-joiner-call.ts`) keep their companion streams with the helpers here,
 * so the rule is written once.
 */

/** What the SFU appends to a peer id to name that peer's light companion. */
export const LIGHT_STREAM_SUFFIX = "~light";

/** A remote MSID stream id, read. */
export interface RemoteStreamId {
  /** The source peer. */
  readonly peerId: string;
  /** Whether the stream is that peer's light companion. */
  readonly light: boolean;
}

/**
 * Split a remote stream id into the peer it belongs to and whether it is the
 * light companion. The bare suffix (no peer id before it) names no peer and
 * is read as an ordinary stream id.
 */
export function splitStreamId(streamId: string): RemoteStreamId {
  if (streamId.length > LIGHT_STREAM_SUFFIX.length && streamId.endsWith(LIGHT_STREAM_SUFFIX)) {
    return { peerId: streamId.slice(0, -LIGHT_STREAM_SUFFIX.length), light: true };
  }
  return { peerId: streamId, light: false };
}

/** The light stream id of a peer (the inverse of {@link splitStreamId}). */
export function lightStreamIdOf(peerId: string): string {
  return `${peerId}${LIGHT_STREAM_SUFFIX}`;
}

/** The peer -> light stream map both call adapters keep. */
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
