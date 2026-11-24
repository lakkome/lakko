// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The composer's one record button: a tap switches the mode, a hold records.
 *
 * ## The defect
 *
 * Web showed a running recorder that was not recording. With permission already
 * granted the machine jumped straight into the recording phase — "granted" was
 * read as "the microphone is in hand" when it only means "no prompt will
 * appear" — and the effect that follows then awaited `getUserMedia` before
 * constructing the `MediaRecorder`. So the bar ran, the clock counted, the red
 * dot pulsed, and whatever the person said in that window was lost.
 *
 * ## What this file adds over `recording-machine.test.ts`
 *
 * The reducer's table is covered there and covered well; it cannot see two
 * things. First, whether the button is wired to the pointer events the tap /
 * hold distinction lives in at all — swap `onPointerDown` for `onClick` and
 * every reducer test still passes while a hold records nothing. Second, what
 * the person is being SHOWN while the device opens: the whole defect was a
 * visible claim ("recording") that was not true yet, and the bar's red dot is
 * that claim. Both are DOM facts.
 *
 * ## The stubs, and what they are allowed to decide
 *
 * jsdom has no `MediaRecorder`, no `getUserMedia` and no `AudioContext`, so the
 * browser's recording stack is stood in for. It records calls and resolves when
 * told; it decides nothing. Every assertion below is about what the production
 * controller, machine and component did to it — notably the ORDER: whether
 * `MediaRecorder.start()` had been called by the time the UI claimed to be
 * recording.
 *
 * Voice only. A video note runs the frame through `canvas.captureStream()`,
 * which jsdom does not implement; that path needs a real browser.
 */

// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSignal, Show } from "solid-js";

import { RecordButton, RecordingBar, recorderBusy } from "./MediaComposer";
import { createRecorder, type Recorder } from "./recording-controller";
import { HOLD_TO_RECORD_MS } from "./recording-machine";
import { setRecordMode } from "./media-prefs";
import { one, pointer, present, renderUi } from "../test/ui";

// ----- the browser's recording stack, stood in for --------------------------

interface Deferred {
  resolve: () => void;
  reject: (e: unknown) => void;
}

let engine: string[] = [];
let pending: Deferred | null = null;
let recorders = 0;

function fakeTrack(): unknown {
  return { kind: "audio", stop: () => engine.push("track.stop"), addEventListener: () => {} };
}

function fakeStream(): unknown {
  const tracks = [fakeTrack()];
  return { getTracks: () => tracks, getAudioTracks: () => tracks, getVideoTracks: () => [] };
}

class FakeRecorder {
  static isTypeSupported(mime: string): boolean {
    // Both formats, so the mode switch has a camera mode to switch TO; a
    // browser that cannot write a video note is `RecordButtonVideoUnsupported`.
    return mime.startsWith("audio/mp4") || mime.startsWith("video/mp4");
  }
  state = "inactive";
  constructor(_stream: unknown, _opts: unknown) {
    recorders += 1;
  }
  addEventListener(): void {}
  start(): void {
    this.state = "recording";
    engine.push("recorder.start");
  }
  stop(): void {
    this.state = "inactive";
    engine.push("recorder.stop");
  }
}

function installEngine(): void {
  engine = [];
  pending = null;
  recorders = 0;
  Object.defineProperty(globalThis, "MediaRecorder", {
    value: FakeRecorder,
    configurable: true,
    writable: true,
  });
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: () =>
        new Promise((res, rej) => {
          engine.push("getUserMedia");
          pending = { resolve: () => res(fakeStream()), reject: rej };
        }),
      // A camera exists, so the mode switch has somewhere to switch TO: with no
      // videoinput the controller pins the mode to voice and a tap is a no-op
      // for a reason that has nothing to do with this defect.
      enumerateDevices: () => Promise.resolve([{ kind: "videoinput" }, { kind: "audioinput" }]),
    },
  });
}

let recorder: Recorder;
let setInCall: (v: boolean) => void;

beforeEach(async () => {
  installEngine();
  // A module-level signal, remembered per device: without this a test that
  // toggled the mode leaks into the next one.
  setRecordMode("voice");
  vi.useFakeTimers();
  const [inCall, setCall] = createSignal(false);
  setInCall = setCall;
  recorder = createRecorder({
    onSend: () => engine.push("send"),
    inCall,
    createUrl: () => "blob:take",
    revokeUrl: () => {},
  });
  // `hasCamera()` is asked at construction and answers on a microtask.
  await vi.advanceTimersByTimeAsync(0);
  // The composer's shape with an empty field, as `routes/Chat.tsx` builds it:
  // the bar replaces the field while a take is in progress, and the record
  // button STAYS MOUNTED through a held take because it holds the pointer
  // capture the gesture runs on. Unmounting it here would model a composer
  // nobody ships and would hide the release event.
  renderUi(() => (
    <>
      <Show when={recorderBusy(recorder)}>
        <RecordingBar recorder={recorder} />
      </Show>
      <RecordButton recorder={recorder} />
    </>
  ));
});

afterEach(() => {
  recorder.dispose();
  vi.useRealTimers();
});

const REC_BTN = "button.lakko-record-btn";
/** The bar's pulsing red dot: the strongest "this is recording" signal it has,
 *  and the one the defect showed while nothing was being captured. */
const REC_DOT = ".lakko-rec-dot";

function down(): void {
  pointer(one(REC_BTN), "pointerdown", { pointerType: "touch", clientX: 200, clientY: 400 });
}

function up(): void {
  pointer(one(REC_BTN), "pointerup", { pointerType: "touch", clientX: 200, clientY: 400 });
}

describe("a tap", () => {
  it("switches the mode and records nothing", async () => {
    expect(one(REC_BTN).getAttribute("aria-label")).toBe("Record voice message");

    down();
    await vi.advanceTimersByTimeAsync(HOLD_TO_RECORD_MS - 50);
    up();
    await vi.advanceTimersByTimeAsync(20);

    expect(one(REC_BTN).getAttribute("aria-label")).toBe("Record video message");
    // Not "no recording was kept": no microphone was ever opened. A tap that
    // opens the device and throws the take away is a tap that lights the
    // recording indicator on a laptop for switching a glyph.
    expect(engine).toEqual([]);
    expect(recorders).toBe(0);
  });

  it("does not put the composer into the recording bar", async () => {
    down();
    await vi.advanceTimersByTimeAsync(HOLD_TO_RECORD_MS - 50);
    up();
    await vi.advanceTimersByTimeAsync(20);
    // `pressing` is deliberately not busy: swapping the field for a bar on
    // every tap is a flicker.
    expect(present(".lakko-rec-bar")).toBe(false);
    expect(present(REC_BTN)).toBe(true);
  });
});

describe("a hold", () => {
  it("shows that the device is opening, and claims nothing more", async () => {
    down();
    await vi.advanceTimersByTimeAsync(HOLD_TO_RECORD_MS + 20);

    // The microphone has been asked for and has not answered.
    expect(engine).toEqual(["getUserMedia"]);
    expect(one(".lakko-rec-bar").textContent).toContain("Starting");
    // THE DEFECT, as a person saw it: the red dot pulsing over a recorder that
    // had not been constructed, let alone started.
    expect(present(REC_DOT)).toBe(false);
    expect(recorders).toBe(0);
  });

  it("records only once the stream is in hand", async () => {
    down();
    await vi.advanceTimersByTimeAsync(HOLD_TO_RECORD_MS + 20);
    pending?.resolve();
    await vi.advanceTimersByTimeAsync(0);

    expect(engine).toEqual(["getUserMedia", "recorder.start"]);
    // Only now. The claim and the capture begin in the same frame, which is the
    // property the fix is: a capturing phase is reachable only on the event that
    // hands the stream over.
    expect(present(REC_DOT)).toBe(true);
    expect(one(".lakko-rec-bar").textContent).toContain("Slide to cancel");
  });

  it("never draws the dot before the recorder has started", async () => {
    down();
    // Step through the whole opening window: at no point may the bar say it is
    // recording while `MediaRecorder.start()` has not been called.
    for (let t = 0; t < HOLD_TO_RECORD_MS + 400; t += 20) {
      await vi.advanceTimersByTimeAsync(20);
      if (present(REC_DOT)) expect(engine).toContain("recorder.start");
    }
    expect(engine).not.toContain("recorder.start");
    expect(present(REC_DOT)).toBe(false);
  });

  it("keeps the mode it recorded in", async () => {
    down();
    await vi.advanceTimersByTimeAsync(HOLD_TO_RECORD_MS + 20);
    pending?.resolve();
    await vi.advanceTimersByTimeAsync(0);
    up();
    await vi.advanceTimersByTimeAsync(0);
    // A hold is not also a tap: the release that ends a recording must not
    // switch the mode on the way out.
    expect(recorder.mode()).toBe("voice");
  });
});

describe("the button's own state", () => {
  it("is disabled, with the reason, while a call owns the microphone", async () => {
    expect(one<HTMLButtonElement>(REC_BTN).disabled).toBe(false);

    setInCall(true);

    const btn = one<HTMLButtonElement>(REC_BTN);
    expect(btn.disabled).toBe(true);
    expect(btn.getAttribute("aria-disabled")).toBe("true");
    expect(btn.closest("span")?.getAttribute("title")).toBe("Unavailable during a call");
    down();
    await vi.advanceTimersByTimeAsync(HOLD_TO_RECORD_MS + 50);
    // A disabled button gets no pointer events from a real engine either; what
    // matters is that nothing here reaches the microphone by another route.
    expect(engine).toEqual([]);
  });

  it("offers the mode switch to a keyboard, which has no hold", async () => {
    // The gesture pair is unreachable without a pointer, so the second control
    // is not decoration: without it the camera cannot be reached at all.
    const swap = one<HTMLButtonElement>("button.lakko-mode-switch-a11y");
    expect(swap.getAttribute("aria-label")).toBe("Switch to video message");
    swap.click();
    await vi.advanceTimersByTimeAsync(0);
    expect(recorder.mode()).toBe("video_note");
    expect(engine).toEqual([]);
  });
});
