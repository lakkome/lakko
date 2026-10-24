// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A group call through a real SFU: three production `SfuCallSession`s in one
 * page (`playwright/harness/group-call-main.ts`) admitted to a
 * `lakko-svc-sfu` this spec starts on loopback, with fake cameras and
 * microphones.
 *
 * The SFU is the one the deployment runs, built from this tree. Its admission
 * keys are the test fixture of `ci/sfu-e2e/test-jwks` (seed `0x42` x 32,
 * public by design), served from a local HTTP server; the tokens come from
 * `lakko-call-jwt`'s `mint_test_jwt` example, which signs with that seed and
 * derives the room from the call id as `services/auth` does. Nothing else is
 * contacted.
 *
 * Every assertion is per pair of people: each participant decodes video
 * frames from, and hears, each other one. A frame sealed under no key, or not
 * sealed, is dropped by the receiving worker before the decoder, so frames
 * decoded is also the proof of the seal end to end through the SFU.
 */

import { expect, test } from "@playwright/test";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { createServer, type Server } from "node:http";
import { createServer as createNetServer } from "node:net";
import { networkInterfaces } from "node:os";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../../../..");
const JWKS = readFileSync(resolve(REPO, "ci/sfu-e2e/test-jwks/jwks.json"), "utf8");
const SFU_BIN = resolve(REPO, "target/debug/lakko-svc-sfu");
const MINT_BIN = resolve(REPO, "target/debug/examples/mint_test_jwt");

interface FromPeer {
  videoFramesDecoded: number;
  videoFramesByTrack: Record<string, number>;
  audioPackets: number;
  audioConcealedRatio: number;
}
interface ParticipantReport {
  state: string;
  peerId: string;
  from: Record<string, FromPeer>;
}
type GroupStep =
  | { do: "wait"; ms: number }
  | { do: "camera"; who: number }
  | { do: "want"; quality: "main" | "middle" | "light" };

/** Enough decoded frames that a picture was shown. */
const SHOWN = 10;

let jwksServer: Server | null = null;
let sfu: ChildProcess | null = null;
let sfuBaseUrl = "";

async function freePort(): Promise<number> {
  return new Promise((ok, fail) => {
    const s = createNetServer();
    s.once("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const address = s.address();
      const port = typeof address === "object" && address !== null ? address.port : 0;
      s.close(() => ok(port));
    });
  });
}

/**
 * The address the SFU's media listens on and advertises: this machine's first
 * non-internal IPv4 address. Not 127.0.0.1, because Firefox pairs no
 * candidate with a loopback address (measured: ICE failed in all three of
 * its sessions while Chromium and WebKit connected); a LAN address is local
 * too, and nothing leaves the machine either way.
 */
function mediaAddress(): string {
  for (const list of Object.values(networkInterfaces())) {
    for (const a of list ?? []) {
      if (a.family === "IPv4" && !a.internal) return a.address;
    }
  }
  return "127.0.0.1";
}

async function waitForHealth(url: string, ms: number): Promise<void> {
  const until = Date.now() + ms;
  for (;;) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // not up yet
    }
    if (Date.now() > until) throw new Error(`the SFU did not answer ${url}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

function mint(callIdHex: string, peerIdHex: string): string {
  return execFileSync(MINT_BIN, ["--call-id", callIdHex, "--peer-id", peerIdHex, "--ttl-secs", "600"], {
    encoding: "utf8",
  });
}

test.describe.configure({ mode: "serial", timeout: 90_000 });
test.use({
  permissions: async ({ browserName }, use) => {
    await use(browserName === "firefox" ? [] : ["camera", "microphone"]);
  },
});

test.beforeAll(async () => {
  test.setTimeout(900_000);
  // The SFU and the token minter, built from this tree (a no-op when fresh).
  execFileSync("cargo", ["build", "-q", "-p", "lakko-svc-sfu", "-p", "lakko-call-jwt", "--example", "mint_test_jwt"], {
    cwd: REPO,
    stdio: "inherit",
  });
  const jwksPort = await freePort();
  jwksServer = createServer((_req, res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JWKS);
  });
  await new Promise<void>((ok) => jwksServer?.listen(jwksPort, "127.0.0.1", ok));
  const [bind, metrics, media] = [await freePort(), await freePort(), await freePort()];
  const mediaIp = mediaAddress();
  sfu = spawn(SFU_BIN, [], {
    cwd: REPO,
    env: {
      ...process.env,
      RUST_LOG: "error",
      LAKKO_SFU_JWKS_URL: `http://127.0.0.1:${jwksPort}/.well-known/jwks.json`,
      LAKKO_SFU_BIND: `127.0.0.1:${bind}`,
      LAKKO_SFU_METRICS_BIND: `127.0.0.1:${metrics}`,
      LAKKO_SFU_MEDIA_BIND: `${mediaIp}:${media}`,
      LAKKO_SFU_MEDIA_ADVERTISE_IP: mediaIp,
    },
    stdio: ["ignore", "inherit", "inherit"],
  });
  sfuBaseUrl = `ws://127.0.0.1:${bind}`;
  await waitForHealth(`http://127.0.0.1:${bind}/health`, 30_000);
});

test.afterAll(async () => {
  sfu?.kill("SIGTERM");
  sfu = null;
  await new Promise<void>((ok) => (jwksServer === null ? ok() : jwksServer.close(() => ok())));
  jwksServer = null;
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

/** A fresh call id and three peer ids, and their tokens. */
function tokens(n: number): string[] {
  const hex = (): string =>
    Array.from({ length: 16 }, () => Math.floor(Math.random() * 256).toString(16).padStart(2, "0")).join("");
  const callId = hex();
  return Array.from({ length: n }, () => mint(callId, hex()));
}

async function groupCall(
  page: import("@playwright/test").Page,
  video: boolean[],
  steps: GroupStep[],
): Promise<ParticipantReport[]> {
  const input = { sfuBaseUrl, jwts: tokens(video.length), video, steps };
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
  const r = await groupCall(page, [true, true, true], [{ do: "wait", ms: 6_000 }]);
  expect(r.map((p) => p.state)).toEqual(["connected", "connected", "connected"]);
  eachPair(r, (got, who) => {
    expect(got?.videoFramesDecoded ?? 0, `${who}: no video`).toBeGreaterThan(SHOWN);
    expect(got?.audioPackets ?? 0, `${who}: no audio`).toBeGreaterThan(0);
    expect(got?.audioConcealedRatio ?? 1, `${who}: audio mostly concealment`).toBeLessThan(0.5);
  });
});

test("three people asking for the main picture of each other get it", async ({ page }) => {
  const r = await groupCall(page, [true, true, true], [
    { do: "wait", ms: 2_000 },
    { do: "want", quality: "main" },
    { do: "wait", ms: 5_000 },
  ]);
  eachPair(r, (got, who) => {
    expect(got?.videoFramesByTrack["main"] ?? 0, `${who}: no main picture`).toBeGreaterThan(SHOWN);
  });
});

test("an audio call where one person turns the camera on: the other two see it", async ({ page }) => {
  const r = await groupCall(page, [false, false, false], [
    { do: "wait", ms: 2_000 },
    { do: "camera", who: 0 },
    { do: "wait", ms: 6_000 },
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
