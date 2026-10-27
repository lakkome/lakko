// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A `lakko-svc-sfu` on loopback for the call specs, and the tokens it admits.
 *
 * The SFU is the one the deployment runs, built from this tree. Its admission
 * keys are the test fixture of `ci/sfu-e2e/test-jwks` (seed `0x42` x 32,
 * public by design), served from a local HTTP server; the tokens come from
 * `lakko-call-jwt`'s `mint_test_jwt` example, which signs with that seed and
 * derives the room from the call id as `services/auth` does. Nothing else is
 * contacted.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { createServer, type Server } from "node:http";
import { createServer as createNetServer } from "node:net";
import { networkInterfaces } from "node:os";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../../../..");
const SFU_BIN = resolve(REPO, "target/debug/lakko-svc-sfu");
const MINT_BIN = resolve(REPO, "target/debug/examples/mint_test_jwt");

export interface SfuStack {
  /** The signaling base URL a session connects to. */
  readonly sfuBaseUrl: string;
  readonly stop: () => Promise<void>;
}

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

/** Builds (a no-op when fresh) and starts the SFU with its JWKS server. */
export async function startSfuStack(): Promise<SfuStack> {
  execFileSync("cargo", ["build", "-q", "-p", "lakko-svc-sfu", "-p", "lakko-call-jwt", "--example", "mint_test_jwt"], {
    cwd: REPO,
    stdio: "inherit",
  });
  const jwks = readFileSync(resolve(REPO, "ci/sfu-e2e/test-jwks/jwks.json"), "utf8");
  const jwksPort = await freePort();
  const jwksServer: Server = createServer((_req, res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(jwks);
  });
  await new Promise<void>((ok) => jwksServer.listen(jwksPort, "127.0.0.1", ok));
  const [bind, metrics, media] = [await freePort(), await freePort(), await freePort()];
  const mediaIp = mediaAddress();
  const sfu: ChildProcess = spawn(SFU_BIN, [], {
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
  await waitForHealth(`http://127.0.0.1:${bind}/health`, 30_000);
  return {
    sfuBaseUrl: `ws://127.0.0.1:${bind}`,
    stop: async () => {
      sfu.kill("SIGTERM");
      await new Promise<void>((ok) => jwksServer.close(() => ok()));
    },
  };
}

/** A fresh call id and `n` peer ids, and their admission tokens. */
export function mintTokens(n: number): string[] {
  const hex = (): string =>
    Array.from({ length: 16 }, () => Math.floor(Math.random() * 256).toString(16).padStart(2, "0")).join("");
  const callId = hex();
  return Array.from({ length: n }, () =>
    execFileSync(MINT_BIN, ["--call-id", callId, "--peer-id", hex(), "--ttl-secs", "600"], { encoding: "utf8" }),
  );
}
