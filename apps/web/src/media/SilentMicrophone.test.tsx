// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec §2 "Hearing nothing" — a take that hears nothing says so while it is
 * being made.
 *
 * ## What this covers that `microphone-silence.test.ts` cannot
 *
 * That file drives the fold directly and proves the rule. It cannot see whether
 * anything ASKS the rule, what it is asked ABOUT, or whether the answer reaches
 * the person — and those are exactly the three joins this warning is made of. A
 * fold nobody calls, a fold called with the meter's ×3 display gain instead of
 * the shared scale, and a verdict that never reaches the bar all leave that file
 * green.
 *
 * So this drives the REAL `createRecorder`, the real `MediaTake` and the real
 * `RecordingBar`, with only the browser's recording stack stood in for, and
 * reads the bar's text. The stub decides nothing except what the microphone
 * hands over; every assertion is about what production code did with it.
 *
 * ## The one thing the stub does decide
 *
 * `installEngine` here adds an `AudioContext` whose analyser fills the buffer
 * with a level this file chooses. `RecordButton.test.tsx` deliberately has no
 * `AudioContext` at all, which is why its takes never warn — and that is a
 * property worth keeping: a take whose meter never ran has measured nothing,
 * and a warning drawn from an absent measurement would accuse a microphone that
 * may be working. The last test below pins it.
 */

// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSignal, Show } from "solid-js";

import { RecordButton, RecordingBar, recorderBusy } from "./MediaComposer";
import { SILENCE_WARNING_MS, SILENT_MIC_NOTICE } from "./microphone-silence";
import { createRecorder, type Recorder } from "./recording-controller";
import { HOLD_TO_RECORD_MS } from "./recording-machine";
import { setRecordMode } from "./media-prefs";
import { one, pointer, renderUi } from "../test/ui";

/** What the fake microphone hands the analyser, sample by sample. */
let micLevel = 0;
let resolveStream: (() => void) | null = null;

function fakeTrack(): unknown {
  return { kind: "audio", stop: () => {}, addEventListener: () => {} };
}

function fakeStream(): unknown {
  const tracks = [fakeTrack()];
  return { getTracks: () => tracks, getAudioTracks: () => tracks, getVideoTracks: () => [] };
}

class FakeRecorder {
  static isTypeSupported(mime: string): boolean {
    return mime.startsWith("audio/mp4");
  }
  state = "inactive";
  addEventListener(): void {}
  start(): void {
    this.state = "recording";
  }
  stop(): void {
    this.state = "inactive";
  }
}

function installEngine(withMeter: boolean): void {
  resolveStream = null;
  Object.defineProperty(globalThis, "MediaRecorder", {
    value: FakeRecorder,
    configurable: true,
    writable: true,
  });
  Object.defineProperty(globalThis, "MediaStream", {
    value: class {
      constructor(_tracks?: unknown) {}
    },
    configurable: true,
    writable: true,
  });
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: () =>
        new Promise((res) => {
          resolveStream = () => res(fakeStream());
        }),
      enumerateDevices: () => Promise.resolve([{ kind: "videoinput" }, { kind: "audioinput" }]),
    },
  });
  if (withMeter) {
    Object.defineProperty(globalThis, "AudioContext", {
      value: class {
        createMediaStreamSource(): { connect: () => void } {
          return { connect: () => {} };
        }
        createAnalyser(): { fftSize: number; getFloatTimeDomainData: (b: Float32Array) => void } {
          return {
            fftSize: 1024,
            getFloatTimeDomainData: (b: Float32Array) => b.fill(micLevel),
          };
        }
        close(): Promise<void> {
          return Promise.resolve();
        }
      },
      configurable: true,
      writable: true,
    });
  } else {
    Reflect.deleteProperty(globalThis as unknown as Record<string, unknown>, "AudioContext");
  }
}

let recorder: Recorder;

async function mount(withMeter: boolean): Promise<void> {
  installEngine(withMeter);
  setRecordMode("voice");
  vi.useFakeTimers();
  const [inCall] = createSignal(false);
  recorder = createRecorder({
    onSend: () => {},
    inCall,
    createUrl: () => "blob:take",
    revokeUrl: () => {},
  });
  await vi.advanceTimersByTimeAsync(0);
  renderUi(() => (
    <>
      <Show when={recorderBusy(recorder)}>
        <RecordingBar recorder={recorder} />
      </Show>
      <RecordButton recorder={recorder} />
    </>
  ));
}

/** Hold past the threshold and let the microphone answer, so a take is running. */
async function startTake(): Promise<void> {
  pointer(one("button.lakko-record-btn"), "pointerdown", {
    pointerType: "touch",
    clientX: 200,
    clientY: 400,
  });
  await vi.advanceTimersByTimeAsync(HOLD_TO_RECORD_MS + 20);
  resolveStream?.();
  await vi.advanceTimersByTimeAsync(0);
}

const barText = (): string => one(".lakko-rec-bar").textContent ?? "";

beforeEach(() => {
  micLevel = 0;
});

afterEach(() => {
  recorder.dispose();
  vi.useRealTimers();
});

describe("a microphone that gives nothing", () => {
  it("says so during the take, in place of the slide hint", async () => {
    await mount(true);
    await startTake();

    // Before the window is up the bar carries its ordinary hint: a microphone
    // that is merely slow to warm up must not be accused.
    await vi.advanceTimersByTimeAsync(SILENCE_WARNING_MS - 300);
    expect(barText()).toContain("Slide to cancel");
    expect(barText()).not.toContain(SILENT_MIC_NOTICE);

    await vi.advanceTimersByTimeAsync(400);
    expect(barText()).toContain(SILENT_MIC_NOTICE);
    expect(barText()).not.toContain("Slide to cancel");
  });

  it("stops saying it the moment the microphone is heard", async () => {
    await mount(true);
    await startTake();
    await vi.advanceTimersByTimeAsync(SILENCE_WARNING_MS + 200);
    expect(barText()).toContain(SILENT_MIC_NOTICE);

    // Unmuted mid-take: the warning describes the present, not a verdict on
    // the take, so it must clear rather than persist.
    micLevel = 0.4;
    await vi.advanceTimersByTimeAsync(300);
    expect(barText()).not.toContain(SILENT_MIC_NOTICE);
    expect(barText()).toContain("Slide to cancel");
  });
});

describe("a microphone that is working", () => {
  it("is never accused, however long the take runs", async () => {
    await mount(true);
    micLevel = 0.3;
    await startTake();
    await vi.advanceTimersByTimeAsync(SILENCE_WARNING_MS * 3);
    expect(barText()).not.toContain(SILENT_MIC_NOTICE);
  });
});

describe("a take with no level meter at all", () => {
  it("accuses nobody, because it has measured nothing", async () => {
    // No `AudioContext`: `MediaTake` skips level sampling entirely, so there is
    // no reading to judge. Zero measurements and a measurement of zero are
    // different answers, and only the second may raise the warning.
    await mount(false);
    await startTake();
    await vi.advanceTimersByTimeAsync(SILENCE_WARNING_MS * 3);
    expect(barText()).not.toContain(SILENT_MIC_NOTICE);
  });
});
