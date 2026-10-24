// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Two production `CallSession`s in one page, A the caller and B the answerer,
 * their signaling handed across through the real wasm codec and each sealing
 * and opening media with the real SFrame transform worker.
 *
 * What a spec reads is what a person sees: whether each side's remote video
 * element has decoded frames. A frame that the receiving worker cannot open is
 * dropped before the decoder (invariant #4), so a side whose peer sent
 * anything unsealed, or sealed under another key, decodes nothing. The audio
 * is read from the receiver statistics the same way.
 */

import { CallSession, type CallSessionCallbacks } from "../../src/calls/call-session";
import { loadWasm } from "../../src/wasm";

type Side = "a" | "b";

/** One step of a scenario, run in order. */
export type CallStep =
  | { readonly do: "dial"; readonly video: boolean }
  | { readonly do: "accept" }
  | { readonly do: "camera"; readonly side: Side }
  | { readonly do: "wait"; readonly ms: number };

export interface SideReport {
  readonly state: string;
  /** Video frames the remote video element has decoded. */
  readonly videoFrames: number;
  readonly videoWidth: number;
  /** Inbound audio packets and how much of the played audio was concealed. */
  readonly audioPackets: number;
  readonly audioConcealedRatio: number;
}

export interface CallReport {
  readonly a: SideReport;
  readonly b: SideReport;
}

const CALL_ID = new Uint8Array(16).fill(7);
const SEED = new Uint8Array(32).fill(9);

async function run(steps: readonly CallStep[]): Promise<CallReport> {
  const wasm = await loadWasm();
  const sessions: Partial<Record<Side, CallSession>> = {};
  const states: Record<Side, string> = { a: "idle", b: "idle" };
  const deliver = (to: Side, bytes: Uint8Array): void => {
    const parsed = wasm.parseCallSignaling(bytes);
    void (async (): Promise<void> => {
      if (to === "b" && sessions.b === undefined) sessions.b = make("b");
      await sessions[to]?.applyIncomingSignaling(parsed);
    })();
  };
  function make(side: Side): CallSession {
    const other: Side = side === "a" ? "b" : "a";
    const video = document.getElementById(`${side}-remote`) as HTMLVideoElement;
    const callbacks: CallSessionCallbacks = {
      onRemoteStream: (stream) => {
        video.srcObject = stream;
        void video.play().catch(() => {});
      },
      onOutboundSignaling: (bytes) => deliver(other, bytes),
      onStateChange: (state) => {
        states[side] = state;
      },
    };
    const session = new CallSession(CALL_ID, callbacks, { ringTimeoutMs: 60_000 });
    session.setCallKey(SEED, side === "a" ? 0 : 1);
    return session;
  }
  sessions.a = make("a");
  for (const step of steps) {
    if (step.do === "dial") await sessions.a.dial({ audio: true, video: step.video });
    else if (step.do === "accept") {
      await waitFor(() => sessions.b !== undefined && states.b === "ringing", "B never rang");
      await sessions.b?.accept();
      await waitFor(() => states.a === "connected" && states.b === "connected", "the call never connected");
    } else if (step.do === "camera") await sessions[step.side]?.addLocalVideo();
    else await new Promise((r) => setTimeout(r, step.ms));
  }
  const report = async (side: Side): Promise<SideReport> => {
    const video = document.getElementById(`${side}-remote`) as HTMLVideoElement;
    const quality = video.getVideoPlaybackQuality();
    const pc = (sessions[side] as unknown as { pc: RTCPeerConnection | null }).pc;
    let audioPackets = 0;
    let samples = 0;
    let concealed = 0;
    if (pc !== null) {
      (await pc.getStats()).forEach((s: Record<string, unknown>) => {
        if (s["type"] === "inbound-rtp" && s["kind"] === "audio") {
          audioPackets += Number(s["packetsReceived"] ?? 0);
          samples += Number(s["totalSamplesReceived"] ?? 0);
          concealed += Number(s["concealedSamples"] ?? 0);
        }
      });
    }
    return {
      state: states[side],
      videoFrames: quality.totalVideoFrames,
      videoWidth: video.videoWidth,
      audioPackets,
      audioConcealedRatio: samples === 0 ? 1 : concealed / samples,
    };
  };
  const out = { a: await report("a"), b: await report("b") };
  await sessions.a.hangUp();
  return out;
}

async function waitFor(ok: () => boolean, what: string, ms = 15_000): Promise<void> {
  const until = Date.now() + ms;
  while (!ok()) {
    if (Date.now() > until) throw new Error(what);
    await new Promise((r) => setTimeout(r, 50));
  }
}

(window as unknown as { __lakkoCall: { run: typeof run } }).__lakkoCall = { run };
document.body.dataset["ready"] = "1";
