// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 S4 — account creation is the one request that still pays a proof
 * of work, and the solve is cancellable (docs/ux/anti-abuse.md section 1):
 * Cancel and leaving the screen terminate the worker and nothing further is
 * sent. Every other auth route lost its proof.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import { AuthClient } from "./client";
import {
  isAbortError,
  POW_CHUNK_ATTEMPTS,
  setPowChunkSolverForTests,
  setPowWorkerFactoryForTests,
  solveInChunks,
  solvePow,
  type PowWorkerLike,
} from "./pow";
import type { PowWorkerRequest, PowWorkerResponse } from "./pow-worker";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  setPowWorkerFactoryForTests(null);
  setPowChunkSolverForTests(null);
});

interface Seen {
  readonly url: string;
  readonly authorization: string | undefined;
  readonly body: Record<string, unknown> | null;
}

function stubFetch(answer: (url: string) => { status: number; json: unknown }): Seen[] {
  const seen: Seen[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const headers = (init?.headers ?? {}) as Record<string, string>;
      seen.push({
        url,
        authorization: headers["authorization"],
        body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null,
      });
      const a = answer(url);
      return new Response(JSON.stringify(a.json), { status: a.status });
    }),
  );
  return seen;
}

/** A worker that records what it was asked and answers only when told. */
function fakeWorker(): {
  factory: () => PowWorkerLike;
  posted: PowWorkerRequest[];
  terminated: () => boolean;
  answer: (r: PowWorkerResponse) => void;
} {
  const posted: PowWorkerRequest[] = [];
  let terminated = false;
  let worker: PowWorkerLike | null = null;
  return {
    posted,
    terminated: () => terminated,
    answer: (r) => worker?.onmessage?.({ data: r } as MessageEvent<PowWorkerResponse>),
    factory: () => {
      worker = {
        onmessage: null,
        onerror: null,
        onmessageerror: null,
        postMessage: (m) => posted.push(m),
        terminate: () => {
          terminated = true;
        },
      };
      return worker;
    },
  };
}

const CHALLENGE = { seed: "AAAAAAAAAAAAAAAAAAAAAA==", effort_bits: 0, expires_in: 300 };

describe("the signup proof of work is cancellable", () => {
  it("a signal already aborted sends nothing at all", async () => {
    const seen = stubFetch(() => ({ status: 500, json: {} }));
    const controller = new AbortController();
    controller.abort();
    const err = await new AuthClient("/api/auth")
      .signup(new TextEncoder().encode("password-1234"), { signal: controller.signal })
      .catch((e: unknown) => e);
    expect(isAbortError(err)).toBe(true);
    expect(seen).toHaveLength(0);
  });

  it("Cancel during the solve terminates the worker and never reaches /signup/start", async () => {
    const worker = fakeWorker();
    setPowWorkerFactoryForTests(worker.factory);
    const seen = stubFetch((url) =>
      url.endsWith("/pow/challenge") ? { status: 200, json: CHALLENGE } : { status: 500, json: {} },
    );
    const controller = new AbortController();
    const pending = new AuthClient("/api/auth")
      .signup(new TextEncoder().encode("password-1234"), { signal: controller.signal })
      .catch((e: unknown) => e);
    await vi.waitFor(() => expect(worker.posted).toHaveLength(1));
    expect(worker.terminated()).toBe(false);
    controller.abort();
    const err = await pending;
    expect(isAbortError(err)).toBe(true);
    expect(worker.terminated()).toBe(true);
    expect(seen.map((s) => s.url)).toEqual(["/api/auth/pow/challenge"]);
  });

  it("a finished solve resolves and ends its worker", async () => {
    const worker = fakeWorker();
    setPowWorkerFactoryForTests(worker.factory);
    const pending = solvePow("seed", 8);
    await vi.waitFor(() => expect(worker.posted).toHaveLength(1));
    worker.answer({ id: worker.posted[0]!.id, kind: "proof", proof: { seed: "s", nonce: "n", solution: "x" } });
    await expect(pending).resolves.toEqual({ seed: "s", nonce: "n", solution: "x" });
    expect(worker.terminated()).toBe(true);
  });

  it("the start carries the proof; the challenge comes from auth", async () => {
    const seen = stubFetch((url) =>
      url.endsWith("/pow/challenge") ? { status: 200, json: CHALLENGE } : { status: 429, json: { error: "signup-rate-limited", retry_after_secs: 30 } },
    );
    const err = await new AuthClient("/api/auth")
      .signup(new TextEncoder().encode("password-1234"))
      .catch((e: unknown) => e);
    expect((err as { status?: number }).status).toBe(429);
    expect(seen.map((s) => s.url)).toEqual(["/api/auth/pow/challenge", "/api/auth/signup/start"]);
    expect(seen[1]?.body).toHaveProperty("pow");
  });
});

describe("every other auth route lost its proof of work", () => {
  /** A login handle on the wire (ADR-0054 S5): base64url of 32 bytes. */
  const HANDLE = "B".repeat(43);

  it("login: straight to /login/start, no pow field", async () => {
    const seen = stubFetch(() => ({ status: 429, json: { error: "too-many-attempts", retry_after_secs: 5 } }));
    await new AuthClient("/api/auth").login(HANDLE, new TextEncoder().encode("pw")).catch(() => undefined);
    expect(seen.map((s) => s.url)).toEqual(["/api/auth/login/start"]);
    expect(seen[0]?.body).not.toHaveProperty("pow");
  });

  it.each([
    ["/totp/recover", (c: AuthClient) => c.totpRecover(HANDLE, "123456")],
    ["/vault/recover/start", (c: AuthClient) => c.wordVaultRecoverStart(HANDLE)],
    ["/vault/recover/finish", (c: AuthClient) => c.wordVaultRecoverFinish(HANDLE, new Uint8Array(32))],
    ["/device/authorize/redeem", (c: AuthClient) => c.deviceAuthorizeRedeem("grant")],
    ["/device/request", (c: AuthClient) => c.deviceRequestCreate(new Uint8Array(32).fill(7))],
  ])("%s: one request, no pow field", async (path, run) => {
    const seen = stubFetch(() => ({ status: 401, json: {} }));
    await run(new AuthClient("/api/auth")).catch(() => undefined);
    expect(seen.map((s) => s.url)).toEqual([`/api/auth${path}`]);
    expect(seen[0]?.body).not.toHaveProperty("pow");
  });

  it.each([
    ["/totp/enroll/start", (c: AuthClient) => c.totpEnrollStart("tok")],
    ["/device/authorize/start", (c: AuthClient) => c.deviceAuthorizeStart("tok")],
  ])("%s: the session only, no body", async (path, run) => {
    const seen = stubFetch(() => ({ status: 401, json: {} }));
    await run(new AuthClient("/api/auth")).catch(() => undefined);
    expect(seen.map((s) => s.url)).toEqual([`/api/auth${path}`]);
    expect(seen[0]?.authorization).toBe("Bearer tok");
    expect(seen[0]?.body).toBeNull();
  });

  it("a limit's Retry-After and code reach the error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(JSON.stringify({ error: "too-many-attempts", retry_after_secs: 90 }), {
          status: 429,
          headers: { "retry-after": "90" },
        }),
      ),
    );
    const err = (await new AuthClient("/api/auth")
      .totpRecover(HANDLE, "123456")
      .catch((e: unknown) => e)) as { errorCode: string; retryAfter: number; retryAfterSecs: number };
    expect(err.errorCode).toBe("too-many-attempts");
    expect(err.retryAfter).toBe(90);
    expect(err.retryAfterSecs).toBe(90);
  });
});

describe("the solve is a loop of chunks (ADR-0054 S5)", () => {
  it("Cancel between chunks stops the loop: no chunk runs after the abort", async () => {
    const controller = new AbortController();
    let calls = 0;
    const solver = (_seed: string, _effort: number, max: number) => {
      calls += 1;
      expect(max).toBe(POW_CHUNK_ATTEMPTS);
      if (calls === 3) controller.abort();
      return undefined;
    };
    const err = await solveInChunks(solver, "seed", 8, { signal: controller.signal }).catch(
      (e: unknown) => e,
    );
    expect(isAbortError(err)).toBe(true);
    expect(calls).toBe(3);
  });

  it("the calling-thread fallback yields between chunks, so a Cancel tap stops the CPU work", async () => {
    setPowWorkerFactoryForTests(null); // no worker: the fallback path
    let calls = 0;
    setPowChunkSolverForTests(() => {
      calls += 1;
      return undefined;
    });
    const controller = new AbortController();
    const pending = solvePow("seed", 8, controller.signal).catch((e: unknown) => e);
    // A macrotask later (a tap), the loop has run a few chunks and yielded.
    await new Promise((resolve) => setTimeout(resolve, 5));
    controller.abort();
    const err = await pending;
    expect(isAbortError(err)).toBe(true);
    const atAbort = calls;
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(calls).toBe(atAbort);
    expect(atAbort).toBeGreaterThan(0);
  });

  it("an exhausted budget is an error, not a hang", async () => {
    const err = await solveInChunks(() => undefined, "seed", 8, { maxAttempts: 3 }).catch(
      (e: unknown) => e,
    );
    expect(err).toBeInstanceOf(Error);
    expect(isAbortError(err)).toBe(false);
  });

  it("over the real wasm, a chunked solve finds a proof at effort 0", async () => {
    const wasm = await import("../../wasm/lakko_ffi_wasm");
    const proof = await solveInChunks(
      (seed, effort, max) => wasm.PowSolver.solveChunk(seed, effort, max),
      CHALLENGE.seed,
      0,
    );
    expect(proof.seed).toBe(CHALLENGE.seed);
    expect(proof.nonce.length).toBeGreaterThan(0);
    expect(proof.solution.length).toBeGreaterThan(0);
  });
});
