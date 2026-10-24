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
import type { VideoQuality } from "../../src/calls/video-wants";

export interface GroupCallInput {
  readonly sfuBaseUrl: string;
  /** One admission token per participant, in order. */
  readonly jwts: readonly string[];
  /** Which participants publish a camera from the start. */
  readonly video: readonly boolean[];
  readonly steps: readonly GroupStep[];
}

export type GroupStep =
  | { readonly do: "wait"; readonly ms: number }
  /** Participant `who` turns its camera on during the call. */
  | { readonly do: "camera"; readonly who: number }
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
}

export interface ParticipantReport {
  readonly state: string;
  /** The ICE server URLs the session's peer connection was given. */
  readonly iceServerUrls: readonly string[];
  readonly peerId: string;
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
  for (let i = 0; i < input.jwts.length; i += 1) {
    states.push("idle");
    selfIds.push("");
    trackPeer.push(new Map());
    const session = new SfuCallSession(
      {
        sfuBaseUrl: input.sfuBaseUrl,
        jwt: input.jwts[i] as string,
        seed: SEED,
        localLeaf: i,
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
          const { peerId, role } = splitStreamId(rt.streamId);
          trackPeer[i]?.set(rt.track.id, { peer: peerId, role });
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
      const from: Record<string, { v: number; byTrack: Record<string, number>; a: number; s: number; c: number }> = {};
      if (pc !== null) {
        (await pc.getStats()).forEach((s: Record<string, unknown>) => {
          if (s["type"] !== "inbound-rtp") return;
          const source = trackPeer[i]?.get(String(s["trackIdentifier"] ?? ""));
          if (source === undefined) return;
          const row = (from[source.peer] ??= { v: 0, byTrack: {}, a: 0, s: 0, c: 0 });
          if (s["kind"] === "video") {
            const n = Number(s["framesDecoded"] ?? 0);
            row.v += n;
            row.byTrack[source.role] = (row.byTrack[source.role] ?? 0) + n;
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
        out[peer] = {
          videoFramesDecoded: r.v,
          videoFramesByTrack: r.byTrack,
          audioPackets: r.a,
          audioConcealedRatio: r.s === 0 ? 1 : r.c / r.s,
        };
      }
      const iceServerUrls = (pc?.getConfiguration().iceServers ?? []).flatMap((s) =>
        typeof s.urls === "string" ? [s.urls] : [...s.urls],
      );
      reports.push({ state: states[i] as string, iceServerUrls, peerId: selfIds[i] as string, from: out });
    }
    return reports;
  }
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
