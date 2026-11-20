// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Supporter codes (`docs/ux/supporters.md` section 1): the field rule, the
 * held place, the redeem outcomes, and the sign-up gate's code entry.
 */
import { describe, expect, it, vi } from "vitest";

import {
  codeLooksComplete,
  dropHeldPlace,
  HELD_PLACE_KEY,
  readHeldPlace,
  redeemErrorOutcome,
  redeemSupporterCode,
  writeHeldPlace,
  type PlaceStorage,
  type SupporterCodeWasm,
} from "./supporter-code";
import {
  SIGNUP_GATE_START,
  signupCodeMaySubmit,
  signupFailureView,
  signupGateReduce,
  type SignupGate,
} from "../auth/signup-closed";
import { AuthHttpError } from "../auth/client";
import { claimUsername, SupporterCodeUsedError } from "../auth/login-handle";

function memoryStorage(): PlaceStorage & { map: Map<string, string> } {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
  };
}

const WINDOW_SECS = 30n * 24n * 3600n;

function fakeWasm(log: { codes: string[]; epochs: bigint[]; finalizeThrows?: boolean }): SupporterCodeWasm {
  class Req {
    constructor(
      readonly code: string,
      readonly epoch: bigint,
    ) {
      if (code === "bad") throw new Error("bad-supporter-code: nope");
      log.codes.push(code);
      log.epochs.push(epoch);
    }
    redeemBodyJson(): string {
      return JSON.stringify({ code: this.code, epoch: Number(this.epoch), blinded_element: "b" });
    }
    finalize(evaluation: string, proof: string, key: string): string {
      if (log.finalizeThrows === true) throw new Error("dleq");
      return `token:${evaluation}:${proof}:${key}`;
    }
    free(): void {}
  }
  return {
    supporterCodeEpochAt: (secs) => secs / WINDOW_SECS,
    betaPlaceWindowSecs: () => WINDOW_SECS,
    SupporterCodeRequest: Req,
  };
}

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status });
}

describe("codeLooksComplete", () => {
  it("counts letters and digits only", () => {
    expect(codeLooksComplete("K7QM-2XRA-9DPF-4WNB")).toBe(true);
    expect(codeLooksComplete("k7qm 2xra 9dpf 4wnb")).toBe(true);
    expect(codeLooksComplete("K7QM2XRA9DPF4WN")).toBe(false);
    expect(codeLooksComplete("K7QM2XRA9DPF4WNBX")).toBe(false);
    expect(codeLooksComplete("")).toBe(false);
  });
});

describe("held place", () => {
  it("round-trips and expires", () => {
    const s = memoryStorage();
    expect(readHeldPlace(s, 0)).toBeNull();
    expect(writeHeldPlace(s, { header: "h", epoch: "3", untilMs: 100 })).toBe(true);
    expect(readHeldPlace(s, 50)).toEqual({ header: "h", epoch: "3", untilMs: 100 });
    expect(readHeldPlace(s, 100)).toBeNull();
    expect(s.map.has(HELD_PLACE_KEY)).toBe(false);
  });

  it("an unreadable value reads as none and is removed", () => {
    const s = memoryStorage();
    s.map.set(HELD_PLACE_KEY, "{not json");
    expect(readHeldPlace(s, 0)).toBeNull();
    expect(s.map.has(HELD_PLACE_KEY)).toBe(false);
  });

  it("a throwing store reads as none", () => {
    const s: PlaceStorage = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
      removeItem: () => {
        throw new Error("blocked");
      },
    };
    expect(readHeldPlace(s, 0)).toBeNull();
    expect(writeHeldPlace(s, { header: "h", epoch: "1", untilMs: 1 })).toBe(false);
    dropHeldPlace(s);
    expect(readHeldPlace(null, 0)).toBeNull();
  });
});

describe("redeemErrorOutcome", () => {
  it("maps the server's answers to the screen's lines", () => {
    expect(redeemErrorOutcome(404, "supporter-code-unknown")).toBe("unknown");
    expect(redeemErrorOutcome(410, "supporter-code-expired")).toBe("unknown");
    expect(redeemErrorOutcome(409, "supporter-code-mismatch")).toBe("unknown");
    expect(redeemErrorOutcome(400, "supporter-code-malformed")).toBe("unknown");
    expect(redeemErrorOutcome(429, "supporter-code-limited")).toBe("limited");
    expect(redeemErrorOutcome(400, "epoch-not-issuable")).toBe("epoch");
    expect(redeemErrorOutcome(503, "supporter-codes-disabled")).toBe("failed");
    expect(redeemErrorOutcome(500, null)).toBe("failed");
  });
});

describe("redeemSupporterCode", () => {
  const NOW = 100 * 30 * 24 * 3600 * 1000 + 5;

  function deps(fetchImpl: (url: string, init?: RequestInit) => Promise<Response>, log = { codes: [] as string[], epochs: [] as bigint[] }) {
    return {
      log,
      deps: {
        wasm: fakeWasm(log),
        fetch: vi.fn(fetchImpl) as unknown as typeof fetch,
        authBaseUrl: "/api/auth",
        nowMs: () => NOW,
      },
    };
  }

  it("redeems, fetches the beta-place key and holds the place", async () => {
    const urls: string[] = [];
    const { deps: d } = deps(async (url, init) => {
      urls.push(url);
      expect(init?.credentials).toBe("omit");
      if (url.endsWith("/redeem")) {
        return json(200, { context: "beta-place", epoch: 100, evaluation: "E", proof: "P" });
      }
      return json(200, { context: "beta-place", epoch: 100, public_key: "K" });
    });
    const out = await redeemSupporterCode(d, "K7QM-2XRA-9DPF-4WNB");
    expect(urls).toEqual(["/api/auth/auth/supporter-code/redeem", "/api/auth/auth/tokens/key/beta-place/100"]);
    expect(out).toEqual({
      kind: "held",
      place: { header: "token:E:P:K", epoch: "100", untilMs: NOW + Number(WINDOW_SECS) * 1000 },
    });
  });

  it("rebuilds the request once at the server's epoch", async () => {
    const log = { codes: [] as string[], epochs: [] as bigint[] };
    let n = 0;
    const { deps: d } = deps(async (url) => {
      if (url.endsWith("/redeem")) {
        n += 1;
        return n === 1
          ? json(400, { error: "epoch-not-issuable", current_epoch: 101 })
          : json(200, { context: "beta-place", epoch: 101, evaluation: "E", proof: "P" });
      }
      return json(200, { context: "beta-place", epoch: 101, public_key: "K" });
    }, log);
    const out = await redeemSupporterCode(d, "code");
    expect(log.epochs).toEqual([100n, 101n]);
    expect(out.kind).toBe("held");
  });

  it("a second epoch refusal is a failure", async () => {
    const { deps: d } = deps(async () => json(400, { error: "epoch-not-issuable", current_epoch: 102 }));
    expect((await redeemSupporterCode(d, "code")).kind).toBe("failed");
  });

  it("unknown, limited, network and a bad proof", async () => {
    expect((await redeemSupporterCode(deps(async () => json(404, { error: "supporter-code-unknown" })).deps, "c")).kind).toBe("unknown");
    expect((await redeemSupporterCode(deps(async () => json(429, { error: "supporter-code-limited" })).deps, "c")).kind).toBe("limited");
    expect(
      (
        await redeemSupporterCode(
          deps(async () => {
            throw new TypeError("offline");
          }).deps,
          "c",
        )
      ).kind,
    ).toBe("failed");
    expect((await redeemSupporterCode(deps(async () => json(200, {})).deps, "bad")).kind).toBe("unknown");
    const log = { codes: [] as string[], epochs: [] as bigint[], finalizeThrows: true };
    const bad = deps(async (url) =>
      url.endsWith("/redeem")
        ? json(200, { context: "beta-place", epoch: 100, evaluation: "E", proof: "P" })
        : json(200, { context: "beta-place", epoch: 100, public_key: "K" }),
      log,
    );
    expect((await redeemSupporterCode(bad.deps, "c")).kind).toBe("failed");
  });

  it("a key for another epoch is a failure", async () => {
    const { deps: d } = deps(async (url) =>
      url.endsWith("/redeem")
        ? json(200, { context: "beta-place", epoch: 100, evaluation: "E", proof: "P" })
        : json(200, { context: "beta-place", epoch: 99, public_key: "K" }),
    );
    expect((await redeemSupporterCode(d, "c")).kind).toBe("failed");
  });
});

describe("sign-up gate: supporter code entry", () => {
  const closed: SignupGate = { view: "closed", checking: false };

  it("opens from the full state and goes back", () => {
    const code = signupGateReduce(closed, { type: "code-open" });
    expect(code).toEqual({ view: "code", checking: false, error: null });
    expect(signupGateReduce(code, { type: "code-back" })).toEqual(closed);
    // Not from the form, not while Try again is asking.
    expect(signupGateReduce(SIGNUP_GATE_START, { type: "code-open" })).toEqual(SIGNUP_GATE_START);
    expect(signupGateReduce({ view: "closed", checking: true }, { type: "code-open" }).view).toBe("closed");
  });

  it("checks, then shows the form or a line under the field", () => {
    let s = signupGateReduce(closed, { type: "code-open" });
    expect(signupCodeMaySubmit(s, false)).toBe(false);
    expect(signupCodeMaySubmit(s, true)).toBe(true);
    s = signupGateReduce(s, { type: "code-submit" });
    expect(s).toEqual({ view: "code", checking: true, error: null });
    expect(signupCodeMaySubmit(s, true)).toBe(false);
    expect(signupGateReduce(s, { type: "code-back" })).toBe(s);
    expect(signupGateReduce(s, { type: "code-result", outcome: "unknown" })).toEqual({
      view: "code",
      checking: false,
      error: "unknown",
    });
    expect(signupGateReduce(s, { type: "code-result", outcome: "held" })).toEqual({ view: "form" });
  });

  it("a late open/closed answer does not take the code entry away", () => {
    const s = signupGateReduce(closed, { type: "code-open" });
    expect(signupGateReduce(s, { type: "answer", open: true })).toBe(s);
    expect(signupGateReduce(s, { type: "answer", open: false })).toBe(s);
  });

  it("a spent place shows the full state with the used line, kept across Try again", () => {
    let s = signupGateReduce(SIGNUP_GATE_START, { type: "used" });
    expect(s).toEqual({ view: "closed", checking: false, used: true });
    s = signupGateReduce(s, { type: "retry" });
    expect(s).toEqual({ view: "closed", checking: true, used: true });
    expect(signupGateReduce(s, { type: "answer", open: false })).toEqual({ view: "closed", checking: false, used: true });
    expect(signupGateReduce(s, { type: "answer", open: true })).toEqual({ view: "form" });
  });

  it("the bind's 403 supporter-code-used reads as used, before discovery", async () => {
    const register = vi.fn(async () => "x");
    const err = await claimUsername({
      username: "alice",
      bind: async () => {
        throw new AuthHttpError(403, "/auth/login-handle", JSON.stringify({ error: "supporter-code-used" }));
      },
      register,
      release: async () => {},
    }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(SupporterCodeUsedError);
    expect(register).not.toHaveBeenCalled();
    expect(signupFailureView(err)).toBe("used");
    expect(
      signupFailureView(new AuthHttpError(403, "/auth/login-handle", JSON.stringify({ error: "registration-closed" }))),
    ).toBe("closed");
  });
});

describe("AuthClient.bindLoginHandle with a held place", () => {
  it("carries the place as x-lakko-anon-token, and none without one", async () => {
    const { AuthClient } = await import("../auth/client");
    const seen: Array<Record<string, string>> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init?: RequestInit) => {
        seen.push({ ...(init?.headers as Record<string, string>) });
        return json(200, {});
      }),
    );
    try {
      const handle = "A".repeat(43);
      await new AuthClient("/api/auth").bindLoginHandle("tok", handle, undefined, "PLACE");
      await new AuthClient("/api/auth").bindLoginHandle("tok", handle);
    } finally {
      vi.unstubAllGlobals();
    }
    expect(seen[0]?.["x-lakko-anon-token"]).toBe("PLACE");
    expect(seen[0]?.["authorization"]).toBe("Bearer tok");
    expect(seen[1]?.["x-lakko-anon-token"]).toBeUndefined();
  });
});
