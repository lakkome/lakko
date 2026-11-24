// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec `docs/ux/media-messages.md` §3 states, "Video format unsupported on
 * this device": a browser whose `MediaRecorder` cannot write H.264 + AAC in MP4
 * is never put into camera mode.
 *
 * ## The defect
 *
 * The tap switched to camera mode regardless, and the button then took the
 * disabled state that belongs to "this browser cannot record": a disabled
 * button receives no pointer events, so the tap that would have switched back
 * to the microphone did nothing either, and voice messages were gone too until
 * the remembered mode was cleared. Android had the same class on the
 * emulator: the tap switched, the hold then did nothing visible.
 *
 * The browser's recording stack is stood in for as in `RecordButton.test.tsx`;
 * here it can write the voice format and not the video one.
 */

// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSignal } from "solid-js";

import { RecordButton, RecorderNotices } from "./MediaComposer";
import { createRecorder, type Recorder } from "./recording-controller";
import { HOLD_TO_RECORD_MS } from "./recording-machine";
import { setRecordMode } from "./media-prefs";
import { one, pointer, renderUi } from "../test/ui";

class VoiceOnlyRecorder {
  static isTypeSupported(mime: string): boolean {
    return mime.startsWith("audio/mp4");
  }
  state = "inactive";
  addEventListener(): void {}
  start(): void {}
  stop(): void {}
}

let recorder: Recorder;
let opened = 0;

beforeEach(async () => {
  opened = 0;
  Object.defineProperty(globalThis, "MediaRecorder", {
    value: VoiceOnlyRecorder,
    configurable: true,
    writable: true,
  });
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: () => {
        opened += 1;
        return new Promise(() => {});
      },
      // A camera IS present: the refusal is about the format, not the device.
      enumerateDevices: () => Promise.resolve([{ kind: "videoinput" }, { kind: "audioinput" }]),
    },
  });
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
      <RecorderNotices recorder={recorder} />
      <RecordButton recorder={recorder} />
    </>
  ));
});

afterEach(() => {
  recorder.dispose();
  vi.useRealTimers();
});

const REC_BTN = "button.lakko-record-btn";
const NOTICE = "Video messages can't be recorded in this browser";

async function tap(): Promise<void> {
  pointer(one(REC_BTN), "pointerdown", { pointerType: "touch", clientX: 200, clientY: 400 });
  await vi.advanceTimersByTimeAsync(HOLD_TO_RECORD_MS - 50);
  pointer(one(REC_BTN), "pointerup", { pointerType: "touch", clientX: 200, clientY: 400 });
  await vi.advanceTimersByTimeAsync(20);
}

describe("a browser that cannot record a video note", () => {
  it("keeps the button on the mic and says why when tapped", async () => {
    await tap();

    expect(one(REC_BTN).getAttribute("aria-label")).toBe("Record voice message");
    expect(recorder.mode()).toBe("voice");
    expect(one(REC_BTN).hasAttribute("disabled")).toBe(false);
    expect(document.body.textContent).toContain(NOTICE);
    expect(opened).toBe(0);
  });

  it("says the same through the screen reader's mode switch", async () => {
    one<HTMLButtonElement>("button.lakko-mode-switch-a11y").click();
    await vi.advanceTimersByTimeAsync(0);

    expect(recorder.mode()).toBe("voice");
    expect(document.body.textContent).toContain(NOTICE);
  });

  it("leaves a remembered camera mode for the mic, still recordable", async () => {
    setRecordMode("video_note");
    await vi.advanceTimersByTimeAsync(0);

    expect(recorder.mode()).toBe("voice");
    expect(recorder.disabledReason()).toBeNull();
    expect(one(REC_BTN).getAttribute("aria-label")).toBe("Record voice message");
  });
});
