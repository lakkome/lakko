// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Several production `SfuCallSession`s in one page, each admitted to a real
 * `lakko-svc-sfu` with a token the spec minted, each sealing and opening
 * media with the real SFrame transform worker.
 *
 * What a spec reads, per participant and per other participant: the video
 * frames decoded from that person (over every track the SFU forwarded for
 * them: main, middle and light) and the audio packets received from them,
 * with how much of the played audio was concealment. A frame the receiving
 * worker cannot open is dropped before the decoder, so frames decoded is also
 * the proof that the sender sealed and the receiver opened.
 */

import { SfuCallSession } from "../../src/calls/sfu-call-session";
import { splitStreamId } from "../../src/calls/light-stream";
import { screenStreamPeerOf } from "../../src/calls/screen-share-tracks";
import { ShareStreams } from "../../src/calls/share-streams";
import type { VideoQuality } from "../../src/calls/video-wants";

export interface GroupCallInput {
  readonly sfuBaseUrl: string;
  /** One admission token per participant, in order. */
  readonly jwts: readonly string[];
  /** Which participants publish a camera from the start. */
  readonly video: readonly boolean[];
  readonly steps: readonly GroupStep[];
  /**
   * The MLS leaf of the first participant in this page, the others following
   * it (0 when absent). A call spread over several pages, one engine each,
   * gives each page its own range so no two senders share a leaf.
   */
  readonly firstLeaf?: number;
}

export type GroupStep =
  | { readonly do: "wait"; readonly ms: number }
  /** Participant `who` turns its camera on during the call. */
  | { readonly do: "camera"; readonly who: number }
  /** Participant `who` shares a screen (a moving canvas) during the call. */
  | { readonly do: "screen"; readonly who: number }
  /** Participant `who` stops sharing its screen. */
  | { readonly do: "unscreen"; readonly who: number }
  /**
   * Wait until every participant in this page has received audio from at
   * least `peers` others, or until `ms` passed. For a call spread over
   * several pages, where the page cannot name the others in advance.
   */
  | { readonly do: "untilHeard"; readonly peers: number; readonly ms: number }
  /**
   * Wait until participant `who` (0 when absent) shows some other
   * participant's share number `nth` (0 the first share it received from
   * them, 1 the next) and has decoded at least `frames` frames of it, or
   * until `ms` passed.
   */
  | { readonly do: "untilShare"; readonly nth: number; readonly frames: number; readonly ms: number; readonly who?: number }
  /** Every participant asks for this picture of every other one. */
  | { readonly do: "want"; readonly quality: VideoQuality }
  /**
   * Wait until participant `who` (every one when absent) has decoded at least
   * `frames` video frames from each participant in `from` (every other one
   * when absent), on `track` (any track when absent), or until `ms` passed.
   * A condition rather than a fixed wait: under load (three engines at once)
   * a renegotiation can take longer, and a fixed wait then fails a call that
   * works.
   */
  | {
      readonly do: "until";
      readonly frames: number;
      readonly ms: number;
      readonly track?: string;
      readonly who?: readonly number[];
      readonly from?: readonly number[];
    };

export interface FromPeer {
  readonly videoFramesDecoded: number;
  /** The same, per track the SFU forwarded: main, middle, light. */
  readonly videoFramesByTrack: Readonly<Record<string, number>>;
  readonly audioPackets: number;
  readonly audioConcealedRatio: number;
  /** The share's video tracks received from this person, in order. */
  readonly screenVideoTracks: number;
  /** How many of those the engine has ended (the SFU stopped their line). */
  readonly screenVideoTracksEnded: number;
  /**
   * Which of those the share tile would show now, as the app decides it
   * (`ShareStreams`): its position in the order above, or -1 for none.
   */
  readonly screenShownIndex: number;
  /** Video frames decoded on the shown share track. */
  readonly screenShownFrames: number;
}

export interface ParticipantReport {
  readonly state: string;
  /** The ICE server URLs the session's peer connection was given. */
  readonly iceServerUrls: readonly string[];
  readonly peerId: string;
  /**
   * The codec the participant's microphone is sent as (`audio/red`,
   * `audio/opus`), or null before negotiation: the first codec of the audio
   * sender's parameters, which is the one the engine sends. Not the outbound
   * statistics: measured in Chromium 153, they name Opus while the answer put
   * RED first and RED was sent.
   */
  readonly audioSendCodec: string | null;
  /** Keyed by the other participant's SFU peer id. */
  readonly from: Record<string, FromPeer>;
}

const SEED = new Uint8Array(32).fill(5);

async function run(input: GroupCallInput): Promise<ParticipantReport[]> {
  const sessions: SfuCallSession[] = [];
  const states: string[] = [];
  const selfIds: string[] = [];
  /** Per participant: remote track id -> the peer it came from, and which
   *  of that peer's tracks it is. */
  const trackPeer: Map<string, { peer: string; role: string }>[] = [];
  /** Per participant: the share video tracks received per peer, in order,
   *  and the stream the app's `ShareStreams` would show for that peer. */
  const screenTracks: Map<string, MediaStreamTrack[]>[] = [];
  const shownShare: Map<string, MediaStream>[] = [];
  const shares: ShareStreams[] = [];
  for (let i = 0; i < input.jwts.length; i += 1) {
    states.push("idle");
    selfIds.push("");
    trackPeer.push(new Map());
    screenTracks.push(new Map());
    const shown = new Map<string, MediaStream>();
    shownShare.push(shown);
    shares.push(
      new ShareStreams((peer, stream, only) => {
        if (stream !== null) shown.set(peer, stream);
        else if (only === undefined || shown.get(peer) === only) shown.delete(peer);
      }),
    );
    const session = new SfuCallSession(
      {
        sfuBaseUrl: input.sfuBaseUrl,
        jwt: input.jwts[i] as string,
        seed: SEED,
        localLeaf: (input.firstLeaf ?? 0) + i,
        media: { audio: true, video: input.video[i] === true },
      },
      {
        onStateChange: (s) => {
          states[i] = s;
        },
        onWelcome: (self) => {
          selfIds[i] = self;
        },
        onRemoteTrack: (rt) => {
          const screenPeer = screenStreamPeerOf(rt.streamId);
          const { peerId, role } =
            screenPeer !== null ? { peerId: screenPeer, role: "screen" } : splitStreamId(rt.streamId);
          trackPeer[i]?.set(rt.track.id, { peer: peerId, role });
          if (screenPeer !== null) {
            shares[i]?.add(screenPeer, rt.track);
            if (rt.track.kind === "video") {
              const list = screenTracks[i]?.get(screenPeer) ?? [];
              list.push(rt.track);
              screenTracks[i]?.set(screenPeer, list);
            }
          }
          // Played, muted, as the app plays a remote stream: an engine that is
          // not asked to play a track's audio decodes none of it.
          if (rt.track.kind === "audio") {
            const sink = document.createElement("audio");
            sink.muted = true;
            sink.srcObject = new MediaStream([rt.track]);
            document.body.append(sink);
            void sink.play().catch(() => {});
          }
        },
      },
    );
    sessions.push(session);
  }
  await Promise.all(sessions.map((s) => s.connect()));
  await waitFor(() => states.every((s) => s === "connected"), "not every participant connected");
  const tiles = new Map<string, object>();
  for (const step of input.steps) {
    if (step.do === "wait") await new Promise((r) => setTimeout(r, step.ms));
    else if (step.do === "camera") await sessions[step.who]?.publishVideo();
    else if (step.do === "screen") await sessions[step.who]?.attachScreenShare(movingCanvas());
    else if (step.do === "unscreen") sessions[step.who]?.detachScreenShare();
    else if (step.do === "untilHeard") {
      const until = Date.now() + step.ms;
      for (;;) {
        const reports = await collect();
        const done = reports.every(
          (r) => Object.values(r.from).filter((f) => f.audioPackets > 0).length >= step.peers,
        );
        if (done || Date.now() > until) break;
        await new Promise((r) => setTimeout(r, 250));
      }
    } else if (step.do === "untilShare") {
      const until = Date.now() + step.ms;
      for (;;) {
        const reports = await collect();
        const done = Object.values(reports[step.who ?? 0]?.from ?? {}).some(
          (f) => f.screenShownIndex === step.nth && f.screenShownFrames >= step.frames,
        );
        if (done || Date.now() > until) break;
        await new Promise((r) => setTimeout(r, 250));
      }
    }
    else if (step.do === "until") {
      const until = Date.now() + step.ms;
      const who = step.who ?? sessions.map((_, i) => i);
      for (;;) {
        const reports = await collect();
        const done = who.every((i) => {
          const from = step.from ?? sessions.map((_, j) => j).filter((j) => j !== i);
          return from.every((j) => {
            const got = reports[i]?.from[selfIds[j] ?? ""];
            const n = step.track === undefined ? got?.videoFramesDecoded : got?.videoFramesByTrack[step.track];
            return (n ?? 0) >= step.frames;
          });
        });
        if (done || Date.now() > until) break;
        await new Promise((r) => setTimeout(r, 250));
      }
    } else {
      sessions.forEach((s, i) => {
        selfIds.forEach((peer, j) => {
          if (i === j || peer === "") return;
          const key = `${i}:${peer}`;
          const tile = tiles.get(key) ?? {};
          tiles.set(key, tile);
          s.reportTileVideoQuality(tile, peer, step.quality);
        });
      });
    }
  }
  const reports = await collect();
  for (const s of sessions) s.hangUp();
  return reports;

  async function collect(): Promise<ParticipantReport[]> {
    const reports: ParticipantReport[] = [];
    for (let i = 0; i < sessions.length; i += 1) {
      const pc = (sessions[i] as unknown as { pc: RTCPeerConnection | null }).pc;
      const from: Record<
        string,
        { v: number; byTrack: Record<string, number>; a: number; s: number; c: number; shown: number }
      > = {};
      const shownTrackIds = new Set<string>();
      for (const stream of shownShare[i]?.values() ?? []) {
        for (const t of stream.getVideoTracks()) shownTrackIds.add(t.id);
      }
      let audioSendCodec: string | null = null;
      if (pc !== null) {
        for (const sender of pc.getSenders()) {
          if (sender.track?.kind !== "audio") continue;
          const mime = sender.getParameters().codecs?.[0]?.mimeType;
          if (mime !== undefined) audioSendCodec = mime.toLowerCase();
        }
        (await pc.getStats()).forEach((s: Record<string, unknown>) => {
          if (s["type"] !== "inbound-rtp") return;
          const source = trackPeer[i]?.get(String(s["trackIdentifier"] ?? ""));
          if (source === undefined) return;
          const row = (from[source.peer] ??= { v: 0, byTrack: {}, a: 0, s: 0, c: 0, shown: 0 });
          if (s["kind"] === "video") {
            const n = Number(s["framesDecoded"] ?? 0);
            row.v += n;
            row.byTrack[source.role] = (row.byTrack[source.role] ?? 0) + n;
            if (shownTrackIds.has(String(s["trackIdentifier"] ?? ""))) row.shown += n;
          }
          if (s["kind"] === "audio") {
            row.a += Number(s["packetsReceived"] ?? 0);
            row.s += Number(s["totalSamplesReceived"] ?? 0);
            row.c += Number(s["concealedSamples"] ?? 0);
          }
        });
      }
      const out: Record<string, FromPeer> = {};
      for (const [peer, r] of Object.entries(from)) {
        const tracks = screenTracks[i]?.get(peer) ?? [];
        const shownVideo = shownShare[i]?.get(peer)?.getVideoTracks()[0];
        out[peer] = {
          videoFramesDecoded: r.v,
          videoFramesByTrack: r.byTrack,
          audioPackets: r.a,
          audioConcealedRatio: r.s === 0 ? 1 : r.c / r.s,
          screenVideoTracks: tracks.length,
          screenVideoTracksEnded: tracks.filter((t) => t.readyState === "ended").length,
          screenShownIndex: shownVideo === undefined ? -1 : tracks.indexOf(shownVideo),
          screenShownFrames: r.shown,
        };
      }
      const iceServerUrls = (pc?.getConfiguration().iceServers ?? []).flatMap((s) =>
        typeof s.urls === "string" ? [s.urls] : [...s.urls],
      );
      reports.push({ state: states[i] as string, iceServerUrls, peerId: selfIds[i] as string, audioSendCodec, from: out });
    }
    return reports;
  }
}

/** A screen stand-in: a canvas that changes every frame, captured. */
function movingCanvas(): MediaStream {
  const canvas = document.createElement("canvas");
  canvas.width = 1280;
  canvas.height = 720;
  const g = canvas.getContext("2d");
  let n = 0;
  const draw = (): void => {
    if (g !== null) {
      g.fillStyle = `hsl(${(n * 7) % 360} 60% 50%)`;
      g.fillRect(0, 0, canvas.width, canvas.height);
      g.fillStyle = "#000";
      g.font = "64px sans-serif";
      g.fillText(`screen ${n}`, 80, 160);
    }
    n += 1;
  };
  draw();
  setInterval(draw, 66);
  return canvas.captureStream(15);
}

async function waitFor(ok: () => boolean, what: string, ms = 20_000): Promise<void> {
  const until = Date.now() + ms;
  while (!ok()) {
    if (Date.now() > until) throw new Error(what);
    await new Promise((r) => setTimeout(r, 50));
  }
}

(window as unknown as { __lakkoGroupCall: { run: typeof run } }).__lakkoGroupCall = { run };
document.body.dataset["ready"] = "1";
