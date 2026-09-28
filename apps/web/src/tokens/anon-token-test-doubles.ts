// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Test doubles for the anonymous-token pool: a fake wasm surface and a fake
 * issuer, so a test gets a real {@link AnonTokenPool} whose tokens are
 * predictable strings (`tok-1`, `tok-2`, ...). Test code only.
 */
import {
  AnonTokenPool,
  type AnonTokenWasm,
  type WasmAnonToken,
  type WasmAnonTokenRequest,
} from "./anon-token-pool";
import type { AnonTokenStore } from "./anon-token-store";

/** The key the fake key route serves. */
export const FAKE_ISSUER_KEY = "issuer-key";

/** One request the fake issuer saw. */
export interface IssuerRequest {
  readonly url: string;
  readonly method: string;
  readonly authorization: string | undefined;
  readonly credentials: RequestCredentials | undefined;
  readonly body: unknown;
}

/** Knobs of the fake issuer. */
export interface FakeIssuerOptions {
  /** Answer the next issuance(s) with these responses first. */
  readonly issueScript?: Array<{ status: number; json: unknown; retryAfter?: string }>;
  /** The key the key route serves (default {@link FAKE_ISSUER_KEY}). */
  readonly keyRouteKey?: string;
  /** The `public_key` the issuance response carries. */
  readonly responseKey?: string;
  /** The key the "issuer" actually evaluated under: `finalize` succeeds only
   *  with this one (default {@link FAKE_ISSUER_KEY}). */
  readonly evaluatedUnder?: string;
  /** Unix ms. */
  readonly now?: () => number;
  /** Persistence (ADR-0054 S5); default none. */
  readonly store?: AnonTokenStore | null;
  /** The session installed at construction; `null` installs none. */
  readonly session?: string | null;
}

/** A fake wasm core: epoch = floor(secs / 86400); a token is live in its own
 *  epoch and the next; `finalize` checks the key it is given. */
export function fakeAnonWasm(evaluatedUnder: () => string): AnonTokenWasm & { finalizeKeys: string[] } {
  let serial = 0;
  const finalizeKeys: string[] = [];
  class Request implements WasmAnonTokenRequest {
    constructor(
      private readonly context: string,
      private readonly epoch: bigint,
      private readonly count: number,
    ) {}
    issueBodyJson(): string {
      return JSON.stringify({
        context: this.context,
        epoch: Number(this.epoch),
        blinded_elements: Array.from({ length: this.count }, (_, i) => `b${i}`),
      });
    }
    finalize(evaluations: string[], _proof: string, publicKey: string): WasmAnonToken[] {
      finalizeKeys.push(publicKey);
      if (publicKey !== evaluatedUnder()) {
        throw new Error("proof-verification: the proof does not verify under that key");
      }
      return evaluations.map(() => {
        serial += 1;
        const token: WasmAnonToken = {
          context: this.context,
          epoch: this.epoch,
          headerValue: `tok-${serial}`,
          free: () => undefined,
        };
        return token;
      });
    }
    free(): void {}
  }
  return {
    finalizeKeys,
    AnonTokenRequest: Request,
    anonTokenEpochAt: (secs, window) => secs / window,
    anonTokenDefaultWindowSecs: () => 86_400n,
    anonTokenIsLive: (tokenEpoch, current) => tokenEpoch === current || tokenEpoch + 1n === current,
    anonTokenHeaderName: () => "x-lakko-anon-token",
    anonTokenMaxBatch: () => 32,
  };
}

/** A real pool over a fake issuer. */
export function fakeTokenPool(options: FakeIssuerOptions = {}): {
  pool: AnonTokenPool;
  issuer: IssuerRequest[];
  wasm: ReturnType<typeof fakeAnonWasm>;
} {
  const issuer: IssuerRequest[] = [];
  const script = [...(options.issueScript ?? [])];
  const keyRouteKey = options.keyRouteKey ?? FAKE_ISSUER_KEY;
  const wasm = fakeAnonWasm(() => options.evaluatedUnder ?? FAKE_ISSUER_KEY);
  const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const headers = (init?.headers ?? {}) as Record<string, string>;
    const url = String(input);
    const body =
      typeof init?.body === "string" ? (JSON.parse(init.body) as unknown) : undefined;
    issuer.push({
      url,
      method: init?.method ?? "GET",
      authorization: headers["authorization"] ?? headers["Authorization"],
      credentials: init?.credentials,
      body,
    });
    const keyMatch = /\/auth\/tokens\/key\/([^/]+)\/(\d+)$/.exec(url);
    if (keyMatch !== null) {
      return new Response(
        JSON.stringify({
          context: keyMatch[1],
          epoch: Number(keyMatch[2]),
          public_key: keyRouteKey,
        }),
        { status: 200 },
      );
    }
    const next = script.shift();
    if (next !== undefined) {
      const h = new Headers();
      if (next.retryAfter !== undefined) h.set("retry-after", next.retryAfter);
      return new Response(JSON.stringify(next.json), { status: next.status, headers: h });
    }
    const req = body as { context: string; epoch: number; blinded_elements: string[] };
    return new Response(
      JSON.stringify({
        context: req.context,
        epoch: req.epoch,
        evaluations: req.blinded_elements.map((b) => `e-${b}`),
        proof: "proof",
        public_key: options.responseKey ?? keyRouteKey,
        remaining: 100,
        epoch_ends_at: 0,
      }),
      { status: 200 },
    );
  }) as unknown as typeof fetch;
  const pool = new AnonTokenPool({
    authBaseUrl: "/api/auth",
    fetchImpl,
    loadWasm: async () => wasm,
    now: options.now ?? (() => 1_700_000_000_000),
    takeWaitMs: 1_000,
    store: options.store ?? null,
  });
  const session = options.session === undefined ? "v1.QUNDT1VOVA.0.tag" : options.session;
  if (session !== null) pool.setSessionToken(session);
  return { pool, issuer, wasm };
}
