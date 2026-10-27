// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The stream a received screen share is shown from (ADR-0083).
 *
 * A share's tracks arrive in the remote stream the browser built for their
 * stream id, and that stream is not the share: the SFU names every share of
 * a peer `<peer>~screen`, so when the peer stops and shares again the new
 * picture is added to the SAME browser stream beside the old one. The old
 * track is never ended (the SFU keeps the line it was forwarded on), so the
 * stream holds two live video tracks and a `<video>` element plays the first:
 * the old, silent one. The share tile then waits for a picture forever.
 *
 * So the share is shown from a stream of its own: per sharer, the newest
 * video track and the newest audio track that came with it. A video track
 * starts a new share (the old share's sound goes with it); an audio track
 * joins the current one. Each change hands the store a NEW stream object, so
 * the tile sets it on its element again and waits for its first frame.
 *
 * No Solid: the store write goes through the handed-in writer; the stream
 * constructor is handed in too, for tests without a DOM.
 */

/** Hands the store a sharer's stream, or `null`; with `only`, removes only
 *  that very stream. */
export type ShareStreamWriter = (peerId: string, stream: MediaStream | null, only?: MediaStream) => void;

interface Shown {
  readonly video: MediaStreamTrack | null;
  readonly audio: MediaStreamTrack | null;
  readonly stream: MediaStream;
}

export class ShareStreams {
  private readonly byPeer = new Map<string, Shown>();

  constructor(
    private readonly write: ShareStreamWriter,
    private readonly makeStream: (tracks: MediaStreamTrack[]) => MediaStream = (tracks) =>
      new MediaStream(tracks),
    /** One diagnostic line (ASCII, no ids); left out, nothing is said. */
    private readonly log?: (line: string) => void,
  ) {}

  /** A track of `peerId`'s share arrived. */
  add(peerId: string, track: MediaStreamTrack): void {
    const prev = this.byPeer.get(peerId);
    this.log?.(
      `call screen share track arrived kind=${track.kind}` +
        (track.kind === "video" && prev?.video != null ? " replaces=video" : ""),
    );
    const video = track.kind === "video" ? track : (prev?.video ?? null);
    const audio = track.kind === "video" ? null : track;
    this.show(peerId, video, audio);
    track.addEventListener("ended", () => this.ended(peerId, track));
  }

  /** `peerId` left: its share goes. */
  drop(peerId: string): void {
    this.byPeer.delete(peerId);
    this.write(peerId, null);
  }

  /** The stream shown for `peerId` now. */
  streamOf(peerId: string): MediaStream | undefined {
    return this.byPeer.get(peerId)?.stream;
  }

  private ended(peerId: string, track: MediaStreamTrack): void {
    const cur = this.byPeer.get(peerId);
    const shown = cur !== undefined && (cur.video === track || cur.audio === track);
    this.log?.(`call screen share track ended kind=${track.kind} shown=${shown ? "yes" : "no"}`);
    if (cur === undefined) return;
    if (cur.video === track || (cur.video === null && cur.audio === track)) {
      this.byPeer.delete(peerId);
      this.write(peerId, null, cur.stream);
    } else if (cur.audio === track) {
      this.show(peerId, cur.video, null);
    }
  }

  private show(peerId: string, video: MediaStreamTrack | null, audio: MediaStreamTrack | null): void {
    const tracks = [video, audio].filter((t): t is MediaStreamTrack => t !== null);
    const stream = this.makeStream(tracks);
    this.byPeer.set(peerId, { video, audio, stream });
    this.write(peerId, stream);
  }
}
