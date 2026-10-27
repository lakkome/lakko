// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A group call between DIFFERENT engines, one page each, through a real SFU
 * (`sfu-stack.ts`): what the one-engine spec cannot see.
 *
 * The case it exists for is redundant audio (ADR-0071 amendment of
 * 2026-10-26). Chromium and WebKit send their microphone as an RFC 2198 RED
 * container, sealed whole; Firefox has no `audio/red`, so the SFU forwards
 * those packets to it under its Opus payload type and its receiving worker
 * must take the primary frame out of the opened container. Before that, a
 * Firefox participant received every packet and decoded nothing: it heard no
 * one in Chrome or Safari, and a one-engine call never shows it.
 *
 * Each test first checks that the sender really sent RED, so a change that
 * quietly stops RED being negotiated fails here instead of passing a test that
 * no longer exercises the path.
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
}
interface ParticipantReport {
  state: string;
  peerId: string;
  audioSendCodec: string | null;
  from: Record<string, FromPeer>;
}

/** How long every participant stays in the call once connected. */
const IN_CALL_MS = 10_000;

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

/** One participant per page, all in one call, microphones only. */
async function call(pages: Page[]): Promise<ParticipantReport[]> {
  const jwts = mintTokens(pages.length);
  const sfuBaseUrl = stack?.sfuBaseUrl ?? "";
  const reports = await Promise.all(
    pages.map(async (page, i) => {
      const input = {
        sfuBaseUrl,
        jwts: [jwts[i]],
        video: [false],
        firstLeaf: i,
        steps: [{ do: "wait", ms: IN_CALL_MS }],
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
