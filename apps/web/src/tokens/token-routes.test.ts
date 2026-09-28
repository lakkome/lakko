// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 S3 — the privacy rule on every anonymous-token route: the request
 * carries the token and NOTHING that names the account (no `Authorization`,
 * no cookies - `credentials: "omit"`), even when the client holds a session.
 * Also: no proof of work, and no `/pow/challenge` request, anywhere but the
 * signup.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import { DiscoveryClient } from "../discovery/client";
import { PushClient } from "../push/client";
import { fakeTokenPool } from "./anon-token-test-doubles";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

interface Seen {
  readonly url: string;
  readonly authorization: string | undefined;
  readonly token: string | undefined;
  readonly credentials: RequestCredentials | undefined;
  readonly body: Record<string, unknown> | null;
}

/** Stub the global `fetch` with per-path JSON answers. */
function stubFetch(answer: (url: string) => { status: number; json: unknown }): Seen[] {
  const seen: Seen[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const headers = (init?.headers ?? {}) as Record<string, string>;
      const auth = Object.entries(headers).find(([k]) => k.toLowerCase() === "authorization");
      seen.push({
        url,
        authorization: auth?.[1],
        token: headers["x-lakko-anon-token"],
        credentials: init?.credentials,
        body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null,
      });
      const a = answer(url);
      return new Response(JSON.stringify(a.json), { status: a.status });
    }),
  );
  return seen;
}

/** A 32-byte ristretto point in base64 that the blinder can finalize against
 *  is not needed: these tests stop at the request. `evaluated` is echoed
 *  back only where the flow continues. */
function expectAnonymous(s: Seen | undefined): void {
  expect(s).toBeDefined();
  expect(s?.authorization).toBeUndefined();
  expect(s?.credentials).toBe("omit");
  expect(s?.token).toMatch(/^tok-\d+$/);
  expect(s?.body).not.toHaveProperty("pow");
}

describe("discovery token routes carry the token and no session", () => {
  function client(): DiscoveryClient {
    // The client HOLDS a session: the token routes must not send it.
    return new DiscoveryClient("https://d.test", {
      sessionToken: () => "session-of-the-account",
      tokens: fakeTokenPool().pool,
    });
  }

  it("POST /discovery/oprf (a lookup)", async () => {
    const seen = stubFetch(() => ({ status: 500, json: {} }));
    await client()
      .lookup("alice_1")
      .catch(() => undefined);
    const oprf = seen.find((s) => s.url.endsWith("/discovery/oprf"));
    expectAnonymous(oprf);
    expect(seen.some((s) => s.url.endsWith("/pow/challenge"))).toBe(false);
  });

  it("POST /discovery/channel/register", async () => {
    const seen = stubFetch(() => ({ status: 200, json: { ok: true } }));
    await client().channelRegister({ oprf_hash: "AA==", channel_id: "AA==" });
    const reg = seen.find((s) => s.url.endsWith("/discovery/channel/register"));
    expectAnonymous(reg);
  });

  it.each([
    ["invite", (c: DiscoveryClient) => c.consumeInvite(new Uint8Array(6), new Uint8Array(160).fill(0x41), new Uint8Array(16))],
    ["call-invite", (c: DiscoveryClient) => c.consumeCallInvite(new Uint8Array(6), new Uint8Array(160).fill(0x41), new Uint8Array(16))],
    ["group-invite", (c: DiscoveryClient) => c.consumeGroupInvite(new Uint8Array(6), new Uint8Array(160).fill(0x41), new Uint8Array(16))],
  ])("POST /discovery/%s/{id}/consume", async (kind, run) => {
    const seen = stubFetch(() => ({
      status: 200,
      json: { used_count: 1, max_uses: 5, already_consumed: false },
    }));
    await expect(run(client())).resolves.toMatchObject({ kind: "consumed" });
    const consume = seen.find((s) => s.url.includes(`/discovery/${kind}/`) && s.url.endsWith("/consume"));
    expectAnonymous(consume);
    expect(consume?.body).toHaveProperty("joiner_tag");
  });

  it("the bearer routes still carry the session, and no proof of work", async () => {
    const seen = stubFetch(() => ({ status: 200, json: { ok: true } }));
    await client().uploadKeyPackage(new Uint8Array(16), new Uint8Array([1]), new Uint8Array(32));
    const up = seen.find((s) => s.url.endsWith("/discovery/keypackage"));
    expect(up?.authorization).toBe("Bearer session-of-the-account");
    expect(up?.token).toBeUndefined();
    expect(up?.body).not.toHaveProperty("pow");
  });
});

describe("push-gw routes carry the token and no session", () => {
  const DEVICE = new Uint8Array(32).fill(1);
  const GROUP = new Uint8Array(32).fill(2);

  it.each([
    ["/push/register", (c: PushClient) => c.register(DEVICE, "web_push", new Uint8Array([1]))],
    ["/push/unregister", (c: PushClient) => c.unregister(DEVICE, "web_push")],
    ["/push/group/subscribe", (c: PushClient) => c.subscribeGroup(DEVICE, GROUP)],
    ["/push/group/unsubscribe", (c: PushClient) => c.unsubscribeGroup(DEVICE, GROUP)],
  ])("POST %s", async (path, run) => {
    const seen = stubFetch(() => ({
      status: 200,
      json: { ok: true, removed: true, inserted: true },
    }));
    await run(new PushClient("https://p.test", { tokens: fakeTokenPool().pool }));
    expect(seen).toHaveLength(1);
    expect(seen[0]?.url).toBe(`https://p.test${path}`);
    expectAnonymous(seen[0]);
  });
});

describe("no proof of work outside the signup", () => {
  /** Every non-test source file under `src/`. */
  function sources(dir: string): string[] {
    const out: string[] = [];
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) out.push(...sources(path));
      else if (/\.(ts|tsx)$/.test(name) && !/\.test\.tsx?$/.test(name)) out.push(path);
    }
    return out;
  }

  it("only the auth client asks for a challenge, and only signup solves one", () => {
    const root = fileURLToPath(new URL("..", import.meta.url));
    const challengeCallers: string[] = [];
    const solvers: string[] = [];
    for (const file of sources(root)) {
      const text = readFileSync(file, "utf8");
      const rel = file.slice(root.length);
      // A quoted route string, not a mention in a comment.
      if (/["'`]\/pow\/challenge["'`]/.test(text)) challengeCallers.push(rel);
      if (/\bsolvePow\(/.test(text) && !rel.endsWith("pow.ts")) solvers.push(rel);
    }
    expect(challengeCallers).toEqual(["auth/client.ts"]);
    expect(solvers).toEqual(["auth/client.ts"]);
    const authClient = readFileSync(join(root, "auth/client.ts"), "utf8");
    expect(authClient.match(/\bsolvePow\(/g)).toHaveLength(1);
  });
});
