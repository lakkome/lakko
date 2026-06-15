// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-26 residual — real-browser SFrame interop over the live SFU (web leg).
 *
 * STATUS: scaffold. This is the starting artifact `ci/sframe-interop/README.md`
 * references. It is NOT yet armed:
 *   1. `@playwright/test` is not a repo dependency (intentionally — adding it is
 *      part of arming this, per the dependency-discipline rule).
 *   2. The harness PAGE that exposes `window.__lakkoSframeInterop` (a thin
 *      wrapper over the real `SfuCallSession`) is not bundled yet — it needs a
 *      vite entry under `apps/web/e2e/sframe-interop/` so the spec can drive a
 *      real browser worker. The flow + assertions below are concrete; the
 *      page-wiring TODOs are the remaining work.
 *
 * It lives under `apps/web/e2e/`, OUTSIDE the web `tsconfig.json` `include`
 * (`["src", "vite.config.ts"]`), so the `tsc --noEmit` gate never tries to
 * compile this (it imports `@playwright/test`, absent until armed). Playwright
 * transpiles its own specs.
 *
 * What it proves once armed: two headless-Chrome peers join ONE SFU call
 * through the real `SfuCallSession` (real `RTCRtpScriptTransform` SFrame
 * worker). Peer A publishes fake-media audio; peer B subscribes with the SAME
 * per-call seed. Because a frame that fails to open is DROPPED before the
 * decoder (invariant #4), B's inbound-RTP `getStats` decoding ONLY if its
 * worker opened the frames A's worker sealed, relayed verbatim by the SFU. A
 * wrong-seed negative run must forward packets but decode ~nothing.
 */

// @ts-expect-error -- @playwright/test is added when this harness is armed.
import { test, expect, chromium } from "@playwright/test";

const SFU_URL = process.env["LAKKO_SFRAME_INTEROP_SFU_URL"] ?? "ws://127.0.0.1:7880";
const JWT = process.env["LAKKO_SFRAME_INTEROP_JWT"] ?? "";

/** Fake-media Chromium so a publisher has an audio track without hardware. */
const FAKE_MEDIA_ARGS = [
  "--use-fake-device-for-media-stream",
  "--use-fake-ui-for-media-stream",
  "--autoplay-policy=no-user-gesture-required",
];

/** The per-call SFrame seed both peers derive (the MLS-exporter stand-in, C-1).
 *  A fixed 32-byte vector keeps the smoke deterministic. */
const CALL_SEED = new Uint8Array(32).fill(0x42);

/** Poll a peer's inbound-audio decode stats until decoded samples climb or the
 *  deadline passes. Returns the final snapshot the assertions read. */
interface DecodeStats {
  packetsReceived: number;
  totalSamplesReceived: number;
  concealmentEvents: number;
}

test.describe("SFrame interop over the live SFU (web leg)", () => {
  test.skip(JWT === "", "LAKKO_SFRAME_INTEROP_JWT not set — arm via the workflow");

  test("subscriber decodes the publisher's SFrame-sealed audio", async () => {
    const browser = await chromium.launch({ args: FAKE_MEDIA_ARGS });
    const pubCtx = await browser.newContext();
    const subCtx = await browser.newContext();
    const pub = await pubCtx.newPage();
    const sub = await subCtx.newPage();

    // TODO(arming): navigate both pages to the bundled harness entry that
    // exposes `window.__lakkoSframeInterop.connect({ baseUrl, jwt, seed,
    // localLeaf, role })` over the real `SfuCallSession`, and
    // `window.__lakkoSframeInterop.inboundDecodeStats()` reading getStats.
    // e.g. await pub.goto("http://127.0.0.1:4173/e2e/sframe-interop/");

    // Publisher A (leaf 7) joins + publishes; subscriber B (leaf 9) joins the
    // SAME call_id (bound by the shared JWT) with the SAME seed.
    await connectPeer(pub, { role: "publisher", localLeaf: 7 });
    await connectPeer(sub, { role: "subscriber", localLeaf: 9 });

    const stats = await waitForDecode(sub);
    // Frames arrived AND decoded -> B's worker opened A's sealed frames.
    expect(stats.packetsReceived).toBeGreaterThan(0);
    expect(stats.totalSamplesReceived).toBeGreaterThan(0);

    await browser.close();
  });

  // TODO(arming): the anti-no-op negative — a subscriber deriving a WRONG seed
  // must show packetsReceived > 0 (the SFU still forwards) but
  // totalSamplesReceived ~ 0 (every frame dropped at the SFrame gate). Without
  // this, a green positive could be a worker that forwards plaintext.
});

/** Drive one peer's connect through the in-page harness. */
async function connectPeer(
  // @ts-expect-error -- Page type comes from @playwright/test once armed.
  page,
  opts: { role: "publisher" | "subscriber"; localLeaf: number },
): Promise<void> {
  await page.evaluate(
    ([baseUrl, jwt, seed, role, localLeaf]: [string, string, number[], string, number]) =>
      // @ts-expect-error -- injected by the harness entry (TODO arming).
      window.__lakkoSframeInterop.connect({
        baseUrl,
        jwt,
        seed: new Uint8Array(seed),
        localLeaf,
        role,
      }),
    [SFU_URL, JWT, Array.from(CALL_SEED), opts.role, opts.localLeaf] as const,
  );
}

/** Poll the subscriber page's inbound decode stats up to ~15 s. */
async function waitForDecode(
  // @ts-expect-error -- Page type comes from @playwright/test once armed.
  page,
): Promise<DecodeStats> {
  const deadline = 15_000;
  const start = Date.now();
  let last: DecodeStats = {
    packetsReceived: 0,
    totalSamplesReceived: 0,
    concealmentEvents: 0,
  };
  while (Date.now() - start < deadline) {
    last = (await page.evaluate(() =>
      // @ts-expect-error -- injected by the harness entry (TODO arming).
      window.__lakkoSframeInterop.inboundDecodeStats(),
    )) as DecodeStats;
    if (last.totalSamplesReceived > 0) return last;
    await page.waitForTimeout(500);
  }
  return last;
}
