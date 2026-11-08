// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  RemoteAudioSinkBinder,
  WebAudioRemotePlayer,
  chooseRemoteAudioPath,
  describeRemoteAudioSink,
  playRefusalOf,
  remoteAudioThroughWebAudio,
  rmsOf,
  sinkSummary,
  type GraphNode,
  type LevelNode,
  type PlaybackContext,
  type SinkElement,
  type SinkReport,
  type SinkStream,
  type SinkTrack,
} from "./remote-audio";

const WKWEBVIEW =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
const MAC_SAFARI =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const CHROME =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const WEBVIEW2 = `${CHROME} Edg/141.0.0.0`;
const FIREFOX = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:140.0) Gecko/20100101 Firefox/140.0";

describe("remote audio path", () => {
  it("plays through web audio in the desktop app on WebKit only", () => {
    expect(remoteAudioThroughWebAudio(true, { userAgent: WKWEBVIEW })).toBe(true);
    expect(remoteAudioThroughWebAudio(true, { userAgent: WEBVIEW2 })).toBe(false);
    expect(remoteAudioThroughWebAudio(true, { userAgent: CHROME })).toBe(false);
    expect(remoteAudioThroughWebAudio(false, { userAgent: MAC_SAFARI })).toBe(false);
    expect(remoteAudioThroughWebAudio(false, { userAgent: CHROME })).toBe(false);
    expect(remoteAudioThroughWebAudio(false, { userAgent: FIREFOX })).toBe(false);
  });

  it("follows the device switch when it names a path, else the default", () => {
    expect(chooseRemoteAudioPath(true, { userAgent: WKWEBVIEW }, null)).toBe("webaudio");
    expect(chooseRemoteAudioPath(true, { userAgent: WKWEBVIEW }, "element")).toBe("element");
    expect(chooseRemoteAudioPath(false, { userAgent: CHROME }, "webaudio")).toBe("webaudio");
    expect(chooseRemoteAudioPath(false, { userAgent: CHROME }, "nonsense")).toBe("element");
  });
});

class FakeStream implements SinkStream {
  constructor(public tracks: SinkTrack[] = [{ readyState: "live", muted: false, enabled: true }]) {}
  getAudioTracks(): readonly SinkTrack[] {
    return this.tracks;
  }
}

class FakeElement implements SinkElement<FakeStream> {
  srcObject: FakeStream | null = null;
  muted = false;
  paused = true;
  readyState = 0;
  currentTime = 0;
  isConnected = true;
  playCalls = 0;
  outcome: "ok" | "refuse" | "throw" | "pending" = "ok";
  private pending: (() => void) | null = null;
  play(): Promise<void> {
    this.playCalls += 1;
    if (this.outcome === "throw") throw new Error("sync");
    if (this.outcome === "refuse") {
      const err = new Error("The request is not allowed by the user agent");
      err.name = "NotAllowedError";
      return Promise.reject(err);
    }
    if (this.outcome === "pending") {
      return new Promise((resolve) => {
        this.pending = () => {
          this.paused = false;
          resolve();
        };
      });
    }
    this.paused = false;
    return Promise.resolve();
  }
  settle(): void {
    this.pending?.();
    this.pending = null;
  }
}

const flush = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

describe("RemoteAudioSinkBinder", () => {
  it("binds and calls play() once the element is in the document", () => {
    const b = new RemoteAudioSinkBinder<FakeStream>(false);
    const el = new FakeElement();
    el.isConnected = false;
    const s = new FakeStream();
    b.sync(el, s);
    expect(el.srcObject).toBeNull();
    expect(el.playCalls).toBe(0);
    el.isConnected = true;
    b.sync(el, s);
    expect(el.srcObject).toBe(s);
    expect(el.playCalls).toBe(1);
    expect(el.muted).toBe(false);
  });

  it("keeps the element muted on the web audio path", () => {
    const b = new RemoteAudioSinkBinder<FakeStream>(true);
    const el = new FakeElement();
    b.sync(el, new FakeStream());
    expect(el.muted).toBe(true);
    expect(el.playCalls).toBe(1);
  });

  it("records a refusal and plays again on the next look while paused", async () => {
    const b = new RemoteAudioSinkBinder<FakeStream>(false);
    const el = new FakeElement();
    el.outcome = "refuse";
    const s = new FakeStream();
    b.sync(el, s);
    await flush();
    expect(b.counts()).toEqual({
      playCalls: 1,
      playRefusals: 1,
      lastRefusal: "NotAllowedError: The request is not allowed by the user agent",
    });
    el.outcome = "ok";
    b.sync(el, s);
    await flush();
    expect(el.playCalls).toBe(2);
    expect(el.paused).toBe(false);
    expect(b.counts().lastRefusal).toBeNull();
    // Playing: a look does not call play() again.
    b.sync(el, s);
    expect(el.playCalls).toBe(2);
  });

  it("does not stack play() calls while one is pending", async () => {
    const b = new RemoteAudioSinkBinder<FakeStream>(false);
    const el = new FakeElement();
    el.outcome = "pending";
    const s = new FakeStream();
    b.sync(el, s);
    b.sync(el, s);
    b.sync(el, s);
    expect(el.playCalls).toBe(1);
    el.settle();
    await flush();
    b.sync(el, s);
    expect(el.playCalls).toBe(1);
  });

  it("counts a synchronous throw as a refusal", () => {
    const b = new RemoteAudioSinkBinder<FakeStream>(false);
    const el = new FakeElement();
    el.outcome = "throw";
    b.sync(el, new FakeStream());
    expect(b.counts().playRefusals).toBe(1);
    expect(b.counts().lastRefusal).toBe("Error: sync");
  });

  it("rebinds when the stream changes and clears the element when it goes", () => {
    const b = new RemoteAudioSinkBinder<FakeStream>(false);
    const el = new FakeElement();
    const a = new FakeStream();
    const c = new FakeStream();
    b.sync(el, a);
    b.sync(el, c);
    expect(el.srcObject).toBe(c);
    expect(el.playCalls).toBe(2);
    b.sync(el, null);
    expect(el.srcObject).toBeNull();
    expect(el.playCalls).toBe(2);
  });
});

describe("sink line", () => {
  const base: SinkReport = {
    label: "group#1",
    path: "element",
    paused: false,
    readyState: 4,
    timeMoving: true,
    currentTime: 12.34,
    playCalls: 1,
    playRefusals: 0,
    lastRefusal: null,
    tracks: [{ readyState: "live", muted: false, enabled: true }],
    contextState: null,
    outputLevel: null,
  };

  it("says paused, play error, whether time moves, tracks", () => {
    expect(describeRemoteAudioSink(base)).toBe(
      "[call] remote audio group#1: path=element paused=false ready=4 time_moving=yes play_refusals=0" +
        " play_error=- audio=live/muted=false/enabled=true ct=12.3 play_calls=1",
    );
    const stuck = { ...base, paused: true, timeMoving: false, playRefusals: 2, lastRefusal: "NotAllowedError: x" };
    expect(describeRemoteAudioSink(stuck)).toContain(
      "paused=true ready=4 time_moving=no play_refusals=2 play_error=NotAllowedError: x",
    );
  });

  it("adds the context and the output level on the web audio path", () => {
    const line = describeRemoteAudioSink({ ...base, path: "webaudio", contextState: "running", outputLevel: 0.0512 });
    expect(line).toContain("path=webaudio");
    expect(line).toContain("ctx=running");
    expect(line).toContain("out_level=0.051");
  });

  it("keeps the moving numbers out of the summary", () => {
    expect(sinkSummary(base)).toBe(sinkSummary({ ...base, currentTime: 99, playCalls: 7, outputLevel: 0.2 }));
    expect(sinkSummary(base)).not.toBe(sinkSummary({ ...base, timeMoving: false }));
    expect(sinkSummary({ ...base, tracks: [] })).toContain("audio=none");
  });

  it("bounds a refusal to one line", () => {
    expect(playRefusalOf({ name: "AbortError", message: "a\nb" })).toBe("AbortError: a b");
    expect(playRefusalOf("x".repeat(500)).length).toBe(160);
  });

  it("measures RMS", () => {
    expect(rmsOf([])).toBe(0);
    expect(rmsOf([0.5, -0.5])).toBeCloseTo(0.5);
  });
});

class FakeNode implements GraphNode {
  readonly outs: GraphNode[] = [];
  disconnected = false;
  connect(d: GraphNode): unknown {
    this.outs.push(d);
    return d;
  }
  disconnect(): void {
    this.disconnected = true;
  }
}

class FakeMeter extends FakeNode implements LevelNode {
  fftSize = 0;
  value = 0.25;
  getFloatTimeDomainData(a: Float32Array<ArrayBuffer>): void {
    a.fill(this.value);
  }
}

class FakeContext implements PlaybackContext<FakeStream> {
  state = "suspended";
  readonly destination = new FakeNode();
  readonly sources: FakeNode[] = [];
  meter = new FakeMeter();
  resumes = 0;
  closed = false;
  createMediaStreamSource(): GraphNode {
    const n = new FakeNode();
    this.sources.push(n);
    return n;
  }
  createAnalyser(): LevelNode {
    return this.meter;
  }
  resume(): Promise<void> {
    this.resumes += 1;
    this.state = "running";
    return Promise.resolve();
  }
  close(): Promise<void> {
    this.closed = true;
    this.state = "closed";
    return Promise.resolve();
  }
}

describe("WebAudioRemotePlayer", () => {
  it("plays each stream into the destination and reads the level sent out", () => {
    const ctxs: FakeContext[] = [];
    const lines: string[] = [];
    const p = new WebAudioRemotePlayer<FakeStream>(() => {
      const c = new FakeContext();
      ctxs.push(c);
      return c;
    }, (l) => lines.push(l));
    const a = p.attach(new FakeStream());
    const b = p.attach(new FakeStream());
    expect(ctxs.length).toBe(1);
    const ctx = ctxs[0]!;
    expect(ctx.sources.length).toBe(2);
    expect(ctx.sources[0]!.outs).toContain(ctx.destination);
    expect(ctx.sources[0]!.outs).toContain(ctx.meter);
    expect(ctx.resumes).toBeGreaterThan(0);
    expect(a?.contextState()).toBe("running");
    expect(a?.level()).toBeCloseTo(0.25);
    expect(lines[0]).toBe("[call] remote audio: web audio context suspended");
    a?.detach();
    a?.detach();
    expect(ctx.sources[0]!.disconnected).toBe(true);
    expect(ctx.closed).toBe(false);
    b?.detach();
    expect(ctx.closed).toBe(true);
    // A new stream after the last one left gets a new context.
    p.attach(new FakeStream());
    expect(ctxs.length).toBe(2);
  });

  it("waits for an audio track", () => {
    const p = new WebAudioRemotePlayer<FakeStream>(() => new FakeContext(), () => undefined);
    expect(p.attach(new FakeStream([]))).toBeNull();
  });

  it("reads no level from a context that is not running", () => {
    const ctx = new FakeContext();
    ctx.resume = (): Promise<void> => {
      ctx.resumes += 1;
      return Promise.resolve();
    };
    const p = new WebAudioRemotePlayer<FakeStream>(() => ctx, () => undefined);
    const a = p.attach(new FakeStream());
    expect(a?.level()).toBeNull();
    p.resume();
    expect(ctx.resumes).toBe(2);
  });

  it("says so and plays nothing when no context can be made", () => {
    const lines: string[] = [];
    const p = new WebAudioRemotePlayer<FakeStream>(() => {
      throw new Error("no audio");
    }, (l) => lines.push(l));
    expect(p.attach(new FakeStream())).toBeNull();
    expect(lines[0]).toBe("[call] remote audio: no web audio context: Error: no audio");
  });
});
