// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A group call between DIFFERENT engines, one page each, through a real SFU
 * (`sfu-stack.ts`): what the one-engine spec cannot see.
 *
 * Two cases. A screen shared, stopped and shared again from one engine and
 * watched from another (ADR-0083): the reported defect was Safari sharing to
 * Chrome, where the second share never appeared. The viewer must end up
 * showing the second share, with frames, and the first share's track must
 * have been ended by the SFU (a stopped line is stopped on every subscriber).
 * With the SFU's line stop disabled the first of these tests fails on "the
 * second share shows no picture" (checked when it was written). The tile
 * replacing a still-live earlier share (`ShareStreams`) is NOT exercised
 * here: with the SFU stopping lines the old track has ended before the new
 * one arrives, even when the share is stopped and started at once, so that
 * path is covered by `share-streams.test.ts` alone.
 *
 * And redundant audio (ADR-0071 amendment of 2026-10-26). Chromium and WebKit send their microphone as an RFC 2198 RED
 * container, sealed whole; Firefox has no `audio/red`, so the SFU forwards
 * those packets to it under its Opus payload type and its receiving worker
 * must take the primary frame out of the opened container. Before that, a
 * Firefox participant received every packet and decoded nothing: it heard no
 * one in Chrome or Safari, and a one-engine call never shows it.
 *
 * Each audio test first checks that the sender really sent RED, so a change
 * that quietly stops RED being negotiated fails here instead of passing a test
 * that no longer exercises the path.
 *
 * Runs once, in the `chromium` project: the Chromium participant is the
 * project's own page (its fake microphone plays `fixtures/mic.wav`), and the
 * Firefox and WebKit participants are browsers this spec launches.
 */

import { expect, firefox, test, webkit, type Browser, type Page } from "@playwright/test";
import { mintTokens, startSfuStack, type SfuStack } from "./sfu-stack";

interface FromPeer {
  audioPackets: number;
  audioConcealedRatio: number;
  screenVideoTracks: number;
  screenVideoTracksEnded: number;
  screenShownIndex: number;
  screenShownFrames: number;
}
type Step =
  | { do: "wait"; ms: number }
  | { do: "screen"; who: number }
  | { do: "unscreen"; who: number }
  | { do: "untilHeard"; peers: number; ms: number }
  | { do: "untilShare"; nth: number; frames: number; ms: number };
interface ParticipantReport {
  state: string;
  peerId: string;
  audioSendCodec: string | null;
  from: Record<string, FromPeer>;
}

/** How long every participant stays in the call once connected. */
const IN_CALL_MS = 10_000;

/** Enough decoded frames that a picture was shown. */
const SHOWN = 10;

let stack: SfuStack | null = null;
const launched: Browser[] = [];

test.describe.configure({ mode: "serial", timeout: 120_000 });

test.beforeAll(async ({ browserName }) => {
  if (browserName !== "chromium") return;
  test.setTimeout(900_000);
  stack = await startSfuStack();
});

test.afterAll(async () => {
  for (const b of launched.splice(0)) await b.close();
  await stack?.stop();
  stack = null;
});

test.beforeEach(({ browserName }) => {
  test.skip(browserName !== "chromium", "launches the other engines itself; runs once, from the chromium project");
});

async function harnessPage(page: Page): Promise<Page> {
  if (process.env["LAKKO_E2E_DEBUG"] !== undefined) {
    page.on("console", (m) => process.stdout.write(`[page] ${m.text().slice(0, 300)}\n`));
  }
  await page.goto("/playwright/harness/group-call.html");
  await page.waitForFunction(() => document.body.dataset["ready"] === "1");
  return page;
}

async function enginePage(engine: "firefox" | "webkit", baseURL: string | undefined): Promise<Page> {
  const browser =
    engine === "firefox"
      ? await firefox.launch({
          firefoxUserPrefs: {
            "media.navigator.streams.fake": true,
            "media.navigator.permission.disabled": true,
          },
        })
      : await webkit.launch();
  launched.push(browser);
  const context = await browser.newContext({
    ...(baseURL === undefined ? {} : { baseURL }),
    permissions: engine === "firefox" ? [] : ["camera", "microphone"],
  });
  return harnessPage(await context.newPage());
}

/** One participant per page, all in one call, microphones only; each page
 *  runs its own steps (all the same when one list is given). */
async function call(pages: Page[], steps: Step[] | Step[][] = [{ do: "wait", ms: IN_CALL_MS }]): Promise<ParticipantReport[]> {
  const jwts = mintTokens(pages.length);
  const sfuBaseUrl = stack?.sfuBaseUrl ?? "";
  const reports = await Promise.all(
    pages.map(async (page, i) => {
      const input = {
        sfuBaseUrl,
        jwts: [jwts[i]],
        video: [false],
        firstLeaf: i,
        steps: Array.isArray(steps[0]) ? (steps as Step[][])[i] : steps,
      };
      const [r] = await page.evaluate(
        async (x) =>
          (window as unknown as { __lakkoGroupCall: { run: (i: unknown) => Promise<ParticipantReport[]> } })
            .__lakkoGroupCall.run(x),
        input,
      );
      return r as ParticipantReport;
    }),
  );
  if (process.env["LAKKO_E2E_DEBUG"] !== undefined) process.stdout.write(`${JSON.stringify(reports)}\n`);
  return reports;
}

function expectHears(listener: ParticipantReport, speaker: ParticipantReport, who: string): void {
  const got = listener.from[speaker.peerId];
  expect(got?.audioPackets ?? 0, `${who}: no audio packets`).toBeGreaterThan(0);
  expect(got?.audioConcealedRatio ?? 1, `${who}: the audio played is mostly concealment`).toBeLessThan(0.5);
}

test("Chromium and Firefox hear each other, Chromium sending redundant audio", async ({ page, baseURL }) => {
  const [chrome, fox] = await call([await harnessPage(page), await enginePage("firefox", baseURL)]);
  if (chrome === undefined || fox === undefined) throw new Error("a participant did not report");
  expect([chrome.state, fox.state]).toEqual(["connected", "connected"]);
  expect(chrome.audioSendCodec, "Chromium did not send RED, so the strip is not exercised").toBe("audio/red");
  expectHears(fox, chrome, "Firefox from Chromium");
  expectHears(chrome, fox, "Chromium from Firefox");
});

test("WebKit and Firefox hear each other, WebKit sending redundant audio", async ({ baseURL }) => {
  const [safari, fox] = await call([await enginePage("webkit", baseURL), await enginePage("firefox", baseURL)]);
  if (safari === undefined || fox === undefined) throw new Error("a participant did not report");
  expect([safari.state, fox.state]).toEqual(["connected", "connected"]);
  expect(safari.audioSendCodec, "WebKit did not send RED, so the strip is not exercised").toBe("audio/red");
  expectHears(fox, safari, "Firefox from WebKit");
  expectHears(safari, fox, "WebKit from Firefox");
});

/** The sharer stops and shares again once it hears the viewer, so the viewer
 *  is in the call for both shares; the viewer leaves once the second share
 *  shows. */
async function reshare(sharer: Page, viewer: Page): Promise<void> {
  const [shared, watched] = await call(
    [sharer, viewer],
    [
      [
        { do: "untilHeard", peers: 1, ms: 30_000 },
        { do: "screen", who: 0 },
        { do: "wait", ms: 5_000 },
        { do: "unscreen", who: 0 },
        { do: "wait", ms: 2_000 },
        { do: "screen", who: 0 },
        { do: "wait", ms: 12_000 },
      ],
      [{ do: "untilShare", nth: 1, frames: SHOWN + 20, ms: 60_000 }],
    ],
  );
  if (shared === undefined || watched === undefined) throw new Error("a participant did not report");
  expect([shared.state, watched.state]).toEqual(["connected", "connected"]);
  const got = watched.from[shared.peerId];
  expect(got?.screenVideoTracks ?? 0, "the viewer did not receive both shares").toBe(2);
  expect(got?.screenShownIndex ?? -1, "the viewer does not show the second share").toBe(1);
  expect(got?.screenShownFrames ?? 0, "the second share shows no picture").toBeGreaterThan(SHOWN);
  expect(got?.screenVideoTracksEnded ?? 0, "the first share's line was not stopped").toBe(1);
}

test("WebKit shares, stops and shares again: Chromium shows the second share", async ({ page, baseURL }) => {
  await reshare(await enginePage("webkit", baseURL), await harnessPage(page));
});

test("Chromium shares, stops and shares again: Firefox shows the second share", async ({ page, baseURL }) => {
  await reshare(await harnessPage(page), await enginePage("firefox", baseURL));
});

test("Firefox shares, stops and shares again: WebKit shows the second share", async ({ baseURL }) => {
  await reshare(await enginePage("firefox", baseURL), await enginePage("webkit", baseURL));
});
