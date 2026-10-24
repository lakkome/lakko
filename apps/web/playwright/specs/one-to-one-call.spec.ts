// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A one-to-one call in a real Chromium: two production `CallSession`s in one
 * page (`playwright/harness/call-main.ts`), each sealing and opening media
 * with the real SFrame transform worker, fake camera and microphone.
 *
 * Every assertion is what a person on the call would notice: the remote video
 * shows moving frames, the remote audio is not concealment. A frame sealed
 * under no key, or not sealed at all, is dropped by the receiving worker
 * before the decoder, so "frames decoded" is also the proof that each sender
 * sealed and each receiver opened.
 *
 * Why it exists: on 2026-10-24 a video call answered with video showed the
 * answerer's camera to nobody in Chrome. The answerer adds its camera to the
 * video transceiver the caller's offer created, and Chromium does not run a
 * send transform set on the sender `addTrack` hands back in that case. Every
 * unit test passed; only a browser could show it.
 */

import { expect, test, type Page } from "@playwright/test";

interface SideReport {
  state: string;
  videoFrames: number;
  videoWidth: number;
  audioPackets: number;
  audioConcealedRatio: number;
}
interface CallReport {
  a: SideReport;
  b: SideReport;
}
type CallStep =
  | { do: "dial"; video: boolean }
  | { do: "accept" }
  | { do: "camera"; side: "a" | "b" }
  | { do: "wait"; ms: number };

/** Enough decoded frames that a picture was shown, not one stray key frame. */
const SHOWN = 10;

test.describe.configure({ timeout: 60_000 });

test.beforeEach(async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "needs Chromium's fake camera and microphone");
  await page.goto("/playwright/harness/call.html");
  await page.waitForFunction(() => document.body.dataset["ready"] === "1");
});

async function call(page: Page, steps: CallStep[]): Promise<CallReport> {
  return page.evaluate(
    async (s) =>
      (window as unknown as { __lakkoCall: { run: (x: unknown) => Promise<CallReport> } }).__lakkoCall.run(s),
    steps,
  );
}

function expectAudio(side: SideReport, who: string): void {
  expect(side.audioPackets, `${who} received no audio`).toBeGreaterThan(0);
  expect(side.audioConcealedRatio, `${who}'s audio was mostly concealment`).toBeLessThan(0.5);
}

test("a video call answered with video: each side sees and hears the other", async ({ page }) => {
  const r = await call(page, [
    { do: "dial", video: true },
    { do: "accept" },
    { do: "camera", side: "b" },
    { do: "wait", ms: 4_000 },
  ]);
  expect(r.b.videoFrames, "the answerer does not see the caller").toBeGreaterThan(SHOWN);
  expect(r.a.videoFrames, "the caller does not see the answerer").toBeGreaterThan(SHOWN);
  expectAudio(r.a, "the caller");
  expectAudio(r.b, "the answerer");
});

test("a video call answered with audio only: the answerer sees the caller, the caller sees nothing", async ({ page }) => {
  const r = await call(page, [{ do: "dial", video: true }, { do: "accept" }, { do: "wait", ms: 4_000 }]);
  expect(r.b.videoFrames, "the answerer does not see the caller").toBeGreaterThan(SHOWN);
  expect(r.a.videoFrames).toBe(0);
  expectAudio(r.a, "the caller");
  expectAudio(r.b, "the answerer");
});

test("an audio call where each side turns its camera on during the call", async ({ page }) => {
  const r = await call(page, [
    { do: "dial", video: false },
    { do: "accept" },
    { do: "wait", ms: 1_000 },
    { do: "camera", side: "a" },
    { do: "wait", ms: 2_000 },
    { do: "camera", side: "b" },
    { do: "wait", ms: 4_000 },
  ]);
  expect(r.b.videoFrames, "the answerer does not see the caller's camera").toBeGreaterThan(SHOWN);
  expect(r.a.videoFrames, "the caller does not see the answerer's camera").toBeGreaterThan(SHOWN);
  expectAudio(r.a, "the caller");
  expectAudio(r.b, "the answerer");
});

test("an audio call: each side hears the other", async ({ page }) => {
  const r = await call(page, [{ do: "dial", video: false }, { do: "accept" }, { do: "wait", ms: 3_000 }]);
  expectAudio(r.a, "the caller");
  expectAudio(r.b, "the answerer");
  expect(r.a.videoFrames).toBe(0);
  expect(r.b.videoFrames).toBe(0);
});
