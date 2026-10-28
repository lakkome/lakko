// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A group call through a real SFU: three production `SfuCallSession`s in one
 * page (`playwright/harness/group-call-main.ts`) admitted to a
 * `lakko-svc-sfu` this spec starts on loopback, with fake cameras and
 * microphones.
 *
 * The SFU and its tokens come from `sfu-stack.ts`; nothing else is contacted.
 *
 * Every assertion is per pair of people: each participant decodes video
 * frames from, and hears, each other one. A frame sealed under no key, or not
 * sealed, is dropped by the receiving worker before the decoder, so frames
 * decoded is also the proof of the seal end to end through the SFU.
 */

import { expect, test } from "@playwright/test";
import { mintTokens, startSfuStack, type SfuStack } from "./sfu-stack";

interface FromPeer {
  videoFramesDecoded: number;
  videoFramesByTrack: Record<string, number>;
  audioPackets: number;
  audioConcealedRatio: number;
}
interface ParticipantReport {
  state: string;
  iceServerUrls: string[];
  peerId: string;
  from: Record<string, FromPeer>;
}
type GroupStep =
  | { do: "wait"; ms: number }
  | { do: "camera"; who: number }
  | { do: "screen"; who: number }
  | { do: "want"; quality: "main" | "middle" | "light" }
  | { do: "until"; frames: number; ms: number; track?: string; who?: number[]; from?: number[] };

/** Enough decoded frames that a picture was shown. */
const SHOWN = 10;

let stack: SfuStack | null = null;
let sfuBaseUrl = "";

test.describe.configure({ mode: "serial", timeout: 90_000 });
test.use({
  permissions: async ({ browserName }, use) => {
    await use(browserName === "firefox" ? [] : ["camera", "microphone"]);
  },
});

test.beforeAll(async () => {
  test.setTimeout(900_000);
  stack = await startSfuStack();
  sfuBaseUrl = stack.sfuBaseUrl;
});

test.afterAll(async () => {
  await stack?.stop();
  stack = null;
});

test.beforeEach(async ({ page, browserName }) => {
  test.skip(
    browserName !== "chromium" && browserName !== "webkit" && browserName !== "firefox",
    "no fake camera configured for this engine",
  );
  if (process.env["LAKKO_E2E_DEBUG"] !== undefined) {
    page.on("console", (m) => process.stdout.write(`[page] ${m.text().slice(0, 300)}\n`));
  }
  await page.goto("/playwright/harness/group-call.html");
  await page.waitForFunction(() => document.body.dataset["ready"] === "1");
});

async function groupCall(
  page: import("@playwright/test").Page,
  video: boolean[],
  steps: GroupStep[],
): Promise<ParticipantReport[]> {
  const input = { sfuBaseUrl, jwts: mintTokens(video.length), video, steps };
  const reports = await page.evaluate(
    async (x) =>
      (window as unknown as { __lakkoGroupCall: { run: (i: unknown) => Promise<ParticipantReport[]> } })
        .__lakkoGroupCall.run(x),
    input,
  );
  // The numbers behind a failure, on request.
  if (process.env["LAKKO_E2E_DEBUG"] !== undefined) process.stdout.write(`${JSON.stringify(reports)}\n`);
  return reports;
}

/** Each participant against each other one. */
function eachPair(r: ParticipantReport[], check: (got: FromPeer | undefined, who: string) => void): void {
  for (const [i, me] of r.entries()) {
    for (const [j, other] of r.entries()) {
      if (i === j) continue;
      check(me.from[other.peerId], `participant ${i} from participant ${j}`);
    }
  }
}

test("three people with cameras: each sees and hears the other two", async ({ page }) => {
  const paths: string[] = [];
  page.on("console", (m) => {
    if (m.text().startsWith("call path: ")) paths.push(m.text());
  });
  const r = await groupCall(page, [true, true, true], [
    { do: "until", frames: SHOWN + 20, ms: 20_000 },
    { do: "wait", ms: 1_000 },
  ]);
  expect(r.map((p) => p.state)).toEqual(["connected", "connected", "connected"]);
  // No TURN is minted here, so no ICE server at all: in particular no public
  // STUN server (until 2026-10-24 the default was Google's).
  expect(r.map((p) => p.iceServerUrls)).toEqual([[], [], []]);
  // Each participant logs its path to the SFU once, with no address: a
  // direct one, never a relay, since no TURN is given here.
  expect(paths.length, paths.join("\n")).toBeGreaterThanOrEqual(3);
  for (const line of paths) expect(line).toMatch(/^call path: local=(host|srflx|prflx)\/udp remote=(host|srflx|prflx)\/udp rtt=(\d+ms|-)$/);
  eachPair(r, (got, who) => {
    expect(got?.videoFramesDecoded ?? 0, `${who}: no video`).toBeGreaterThan(SHOWN);
    expect(got?.audioPackets ?? 0, `${who}: no audio`).toBeGreaterThan(0);
    expect(got?.audioConcealedRatio ?? 1, `${who}: audio mostly concealment`).toBeLessThan(0.5);
  });
});

test("three people asking for the main picture of each other get it", async ({ page }) => {
  const r = await groupCall(page, [true, true, true], [
    { do: "until", frames: SHOWN, ms: 20_000 },
    { do: "want", quality: "main" },
    { do: "until", frames: SHOWN + 20, ms: 20_000, track: "main" },
  ]);
  eachPair(r, (got, who) => {
    expect(got?.videoFramesByTrack["main"] ?? 0, `${who}: no main picture`).toBeGreaterThan(SHOWN);
  });
});

test("an audio call where one person turns the camera on: the other two see it", async ({ page }) => {
  const r = await groupCall(page, [false, false, false], [
    { do: "wait", ms: 2_000 },
    { do: "camera", who: 0 },
    { do: "until", frames: SHOWN + 20, ms: 20_000, who: [1, 2], from: [0] },
    { do: "wait", ms: 1_000 },
  ]);
  const camera = r[0]?.peerId ?? "";
  for (const i of [1, 2]) {
    expect(r[i]?.from[camera]?.videoFramesDecoded ?? 0, `participant ${i} does not see the camera`).toBeGreaterThan(
      SHOWN,
    );
  }
  eachPair(r, (got, who) => {
    expect(got?.audioPackets ?? 0, `${who}: no audio`).toBeGreaterThan(0);
  });
});

test("one person shares a screen: the other two see it beside the camera", async ({ page }) => {
  const r = await groupCall(page, [true, false, false], [
    { do: "until", frames: SHOWN, ms: 20_000, who: [1, 2], from: [0] },
    { do: "screen", who: 0 },
    { do: "until", frames: SHOWN + 20, ms: 20_000, track: "screen", who: [1, 2], from: [0] },
    { do: "wait", ms: 1_000 },
  ]);
  const sharer = r[0]?.peerId ?? "";
  for (const i of [1, 2]) {
    expect(
      r[i]?.from[sharer]?.videoFramesByTrack["screen"] ?? 0,
      `participant ${i} does not see the shared screen`,
    ).toBeGreaterThan(SHOWN);
    const byTrack = r[i]?.from[sharer]?.videoFramesByTrack ?? {};
    const camera = Object.entries(byTrack)
      .filter(([track]) => track !== "screen")
      .reduce((n, [, frames]) => n + frames, 0);
    expect(camera, `participant ${i} lost the camera`).toBeGreaterThan(SHOWN);
  }
});
