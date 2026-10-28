// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The browser engine adapters against stand-ins of the browser APIs. What a
 * real browser does is NOT reachable here (no browser engine runs in the test
 * project); what is: the mapping of the browser's answers to an engine state,
 * that a pack download starts only through `translate`, and that a detector
 * which would need a download is never created.
 */

import { describe, expect, it } from "vitest";

import { browserDetection, browserTranscription, browserTranslation, transcriptOf } from "./browser-engines";
import { onCaptureStart, watchCaptures } from "./capture-watch";
import { engineStateOf, permilleOf } from "./engines";

describe("engine state mapping", () => {
  it("maps the four browser answers and refuses anything else", () => {
    expect(engineStateOf("available")).toBe("ready");
    expect(engineStateOf("downloadable")).toBe("needs-pack");
    expect(engineStateOf("downloading")).toBe("needs-pack");
    expect(engineStateOf("unavailable")).toBe("unavailable");
    expect(engineStateOf("readily")).toBe("unavailable");
    expect(engineStateOf(undefined)).toBe("unavailable");
  });

  it("clamps confidence into thousandths", () => {
    expect(permilleOf(0.95)).toBe(950);
    expect(permilleOf(2)).toBe(1000);
    expect(permilleOf(Number.NaN)).toBe(0);
  });
});

describe("browserTranslation", () => {
  it("is absent without the API", () => {
    expect(browserTranslation(null).present).toBe(false);
  });

  it("asks availability without creating, and creates only to translate", async () => {
    const created: string[] = [];
    const api = {
      availability: () => Promise.resolve("downloadable"),
      create: (o: { sourceLanguage: string; targetLanguage: string; monitor?: (m: { addEventListener: (t: string, l: (e: { loaded: number }) => void) => void }) => void }) => {
        created.push(`${o.sourceLanguage}>${o.targetLanguage}`);
        o.monitor?.({ addEventListener: (_t, l) => l({ loaded: 0.5 }) });
        return Promise.resolve({ translate: (text: string) => Promise.resolve(`[${text}]`) });
      },
    };
    const e = browserTranslation(api as never);
    expect(await e.state("es", "en")).toBe("needs-pack");
    expect(created).toEqual([]);
    const progress: number[] = [];
    expect(await e.translate("es", "en", "hola", { onProgress: (f) => progress.push(f) })).toBe("[hola]");
    expect(created).toEqual(["es>en"]);
    expect(progress).toEqual([0.5]);
    // The translator is reused for the pair.
    await e.translate("es", "en", "adios");
    expect(created).toEqual(["es>en"]);
  });

  it("reports unavailable when the browser throws", async () => {
    const api = { availability: () => Promise.reject(new Error("x")), create: () => Promise.reject(new Error("x")) };
    expect(await browserTranslation(api as never).state("es", "en")).toBe("unavailable");
  });
});

describe("browserDetection", () => {
  it("never creates a detector that would need a download", async () => {
    let created = 0;
    const api = {
      availability: () => Promise.resolve("downloadable"),
      create: () => {
        created++;
        return Promise.resolve({ detect: () => Promise.resolve([]) });
      },
    };
    expect(await browserDetection(api as never).detect("hola")).toBeNull();
    expect(created).toBe(0);
  });

  it("returns the best guess of a present detector, and nothing for und", async () => {
    const results: Array<Array<{ detectedLanguage: string; confidence: number }>> = [
      [{ detectedLanguage: "es", confidence: 0.91 }],
      [{ detectedLanguage: "und", confidence: 0.99 }],
    ];
    const api = {
      availability: () => Promise.resolve("available"),
      create: () => Promise.resolve({ detect: () => Promise.resolve(results.shift() ?? []) }),
    };
    const d = browserDetection(api as never);
    expect(await d.detect("hola")).toEqual({ tag: "es", permille: 910 });
    expect(await d.detect("???")).toBeNull();
  });
});

describe("transcriptOf", () => {
  it("joins the final words and the interim tail", () => {
    const results = {
      length: 2,
      0: { isFinal: true, 0: { transcript: "hola " } },
      1: { isFinal: false, 0: { transcript: "amig" } },
    };
    expect(transcriptOf(results as never)).toEqual({ text: "hola amig", final: "hola" });
  });
});

describe("the microphone guard (capture-watch.ts)", () => {
  /** A Web Audio stand-in: decoding succeeds, the source never ends on its own. */
  function graph() {
    const track = {
      stopped: false,
      stop(): void {
        this.stopped = true;
      },
    };
    const source = {
      buffer: null as unknown,
      onended: null as (() => void) | null,
      connect(): void {},
      start(): void {},
      stop(): void {},
    };
    return {
      track,
      factory: () => ({
        state: "running",
        resume: () => Promise.resolve(),
        close: () => Promise.resolve(),
        decodeAudioData: () => Promise.resolve({} as AudioBuffer),
        createBufferSource: () => source as unknown as AudioBufferSourceNode,
        createMediaStreamDestination: () =>
          ({ stream: { getAudioTracks: () => [track] } }) as unknown as MediaStreamAudioDestinationNode,
      }),
    };
  }

  /** A recognizer that keeps the local flag and reports `error` after start, if given. */
  function recognizerApi(errorOnStart: string | null): never {
    class Fake {
      private local = false;
      get processLocally(): boolean {
        return this.local;
      }
      set processLocally(v: boolean) {
        this.local = v;
      }
      lang = "";
      continuous = false;
      interimResults = false;
      maxAlternatives = 0;
      onresult: unknown = null;
      onerror: ((e: { error?: string }) => void) | null = null;
      onend: (() => void) | null = null;
      start(): void {
        if (errorOnStart !== null) {
          setTimeout(() => {
            this.onerror?.({ error: errorOnStart });
            this.onend?.();
          }, 0);
        }
      }
      stop(): void {
        this.onend?.();
      }
      abort(): void {
        setTimeout(() => this.onend?.(), 0);
      }
      static available(): Promise<unknown> {
        return Promise.resolve("available");
      }
      static install(): Promise<unknown> {
        return Promise.resolve(true);
      }
    }
    return Fake as never;
  }

  it("ends a transcription when a capture starts during it", async () => {
    const g = graph();
    let fire: () => void = () => undefined;
    const engine = browserTranscription(recognizerApi(null), g.factory as never, (l) => {
      fire = l;
      return () => undefined;
    });
    const running = engine.transcribe(new Blob([new Uint8Array([1])]), "en");
    for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
    fire();
    await expect(running).rejects.toMatchObject({ name: "MicrophoneGuard" });
    expect(g.track.stopped).toBe(true);
  });

  it("treats a recognizer asking for the microphone as having listened to it", async () => {
    const g = graph();
    const engine = browserTranscription(recognizerApi("not-allowed"), g.factory as never, () => () => undefined);
    await expect(engine.transcribe(new Blob([new Uint8Array([1])]), "en")).rejects.toMatchObject({
      name: "MicrophoneGuard",
    });
  });

  it("does not fire for an ordinary failure", async () => {
    const g = graph();
    const engine = browserTranscription(recognizerApi("network"), g.factory as never, () => () => undefined);
    await expect(engine.transcribe(new Blob([new Uint8Array([1])]), "en")).rejects.toMatchObject({
      message: "recognition-network",
    });
  });
});

describe("watchCaptures", () => {
  it("reports a getUserMedia call and still makes it", async () => {
    const calls: unknown[] = [];
    const devices = {
      getUserMedia: (c?: unknown) => {
        calls.push(c);
        return Promise.resolve({} as MediaStream);
      },
    };
    const g = globalThis as unknown as Record<string, unknown>;
    const saved = g["navigator"];
    Object.defineProperty(globalThis, "navigator", { value: { mediaDevices: devices }, configurable: true });
    try {
      watchCaptures();
      let seen = 0;
      const stop = onCaptureStart(() => seen++);
      await devices.getUserMedia({ audio: true });
      stop();
      await devices.getUserMedia({ audio: true });
      expect(seen).toBe(1);
      expect(calls.length).toBe(2);
    } finally {
      Object.defineProperty(globalThis, "navigator", { value: saved, configurable: true });
    }
  });
});
