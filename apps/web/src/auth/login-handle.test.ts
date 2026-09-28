// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 S5 — sign in and recover by name. Every name route carries the
 * login handle (base64url of 32 bytes) and no account id; nothing looks the
 * name up before a session; a claim binds the handle at auth before it
 * registers the name at discovery; every sign-in by name re-binds.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { AuthClient, AuthHttpError } from "./client";
import { DiscoveryHttpError } from "../discovery/client";
import { ContactKind } from "./wire";
import {
  accessByName,
  claimUsername,
  LoginHandleUsernameError,
  loginHandleWith,
  nameTargetWith,
  UsernameTakenError,
} from "./login-handle";
import { USERNAME_REASON } from "../profile/rules";
import { signupErrorMessage } from "./auth-error-copy";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

interface Seen {
  readonly url: string;
  readonly method: string;
  readonly authorization: string | undefined;
  readonly body: Record<string, unknown> | null;
}

function stubFetch(answer: (url: string, method: string) => { status: number; json: unknown }): Seen[] {
  const seen: Seen[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      const headers = (init?.headers ?? {}) as Record<string, string>;
      seen.push({
        url,
        method,
        authorization: headers["authorization"],
        body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null,
      });
      const a = answer(url, method);
      return new Response(JSON.stringify(a.json), { status: a.status });
    }),
  );
  return seen;
}

const HANDLE_WIRE = /^[A-Za-z0-9_-]{43}$/;

function decodeB64Url(s: string): Uint8Array {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4);
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}

describe("the handle", () => {
  it("is 32 bytes on the wire as base64url without padding", () => {
    const h = loginHandleWith(wasm, "alice");
    expect(h.bytes).toHaveLength(32);
    expect(h.wire).toMatch(HANDLE_WIRE);
    expect(decodeB64Url(h.wire)).toEqual(h.bytes);
  });

  it("is a function of the canonical name (trim, @, case)", () => {
    expect(loginHandleWith(wasm, "  @Alice ").wire).toBe(loginHandleWith(wasm, "alice").wire);
    expect(loginHandleWith(wasm, "bob").wire).not.toBe(loginHandleWith(wasm, "alice").wire);
    expect(nameTargetWith(wasm, "@Alice").username).toBe("alice");
  });

  it("a name the rule refuses throws the username-rule copy", () => {
    const cases: Array<[string, keyof typeof USERNAME_REASON]> = [
      ["ab", "tooShort"],
      ["a".repeat(40), "tooLong"],
      ["al ice", "invalidCharacters"],
    ];
    for (const [name, code] of cases) {
      let err: unknown;
      try {
        loginHandleWith(wasm, name);
      } catch (e) {
        err = e;
      }
      expect(err).toBeInstanceOf(LoginHandleUsernameError);
      expect((err as LoginHandleUsernameError).usernameError).toBe(code);
      expect((err as Error).message).toBe(USERNAME_REASON[code]);
    }
  });
});

describe("every name route sends the handle and no account id", () => {
  const handle = (): string => loginHandleWith(wasm, "alice").wire;

  function assertHandleOnly(body: Record<string, unknown> | null): void {
    expect(body).not.toBeNull();
    expect(body?.["handle"]).toBe(handle());
    expect(body).not.toHaveProperty("account_id");
    expect(body).not.toHaveProperty("account_id_b64");
  }

  it.each([
    ["/login/start", (c: AuthClient) => c.login(handle(), new TextEncoder().encode("pw"))],
    ["/password/reset/start", (c: AuthClient) => c.recover(handle(), new Uint8Array(64).fill(3), new TextEncoder().encode("new-password"))],
    ["/totp/recover", (c: AuthClient) => c.totpRecover(handle(), "123456")],
    ["/vault/recover/start", (c: AuthClient) => c.wordVaultRecoverStart(handle())],
    ["/vault/recover/finish", (c: AuthClient) => c.wordVaultRecoverFinish(handle(), new Uint8Array(32))],
    ["/recovery-contact/recover/start", (c: AuthClient) => c.recoveryContactRecoverStart(handle(), ContactKind.Email, "a@b.c")],
    ["/recovery-contact/recover/finish", (c: AuthClient) => c.recoveryContactRecoverFinish(handle(), ContactKind.Email, "123456")],
  ])("%s", async (path, run) => {
    const seen = stubFetch(() => ({ status: 401, json: {} }));
    await run(new AuthClient("/api/auth")).catch(() => undefined);
    expect(seen.map((s) => s.url)).toEqual([`/api/auth${path}`]);
    assertHandleOnly(seen[0]?.body ?? null);
  });

  it("recovery login start (the shared start shape) carries the handle", async () => {
    const seen = stubFetch(() => ({ status: 401, json: {} }));
    await new AuthClient("/api/auth")
      .recoveryLoginStart({ handle: handle(), request: "cmVx" })
      .catch(() => undefined);
    expect(seen[0]?.url).toBe("/api/auth/recovery/login/start");
    assertHandleOnly(seen[0]?.body ?? null);
  });

  it("a malformed handle never leaves the device", async () => {
    const seen = stubFetch(() => ({ status: 200, json: {} }));
    const client = new AuthClient("/api/auth");
    await expect(client.totpRecover("not-a-handle", "123456")).rejects.toThrow(/handle/);
    await expect(client.wordVaultRecoverStart(`${"A".repeat(43)}=`)).rejects.toThrow(/handle/);
    expect(seen).toHaveLength(0);
  });
});

describe("recover by name: no lookup before the session, the id from the finish, then a re-bind", () => {
  const ACCOUNT = new Uint8Array(16).map((_, i) => i + 1);
  const ACCOUNT_B64URL = btoa(String.fromCharCode(...ACCOUNT)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

  it("TOTP: /totp/recover, then PUT /auth/login-handle with the new session; nothing at discovery", async () => {
    const seen = stubFetch((url) =>
      url.endsWith("/totp/recover")
        ? { status: 200, json: { account_id_b64: ACCOUNT_B64URL, session_token: "sess", session_expires_at_ms: 1 } }
        : { status: 200, json: { bound: true } },
    );
    const client = new AuthClient("/api/auth");
    const target = nameTargetWith(wasm, "@Alice");
    const result = await accessByName({
      target,
      run: (h) => client.totpRecover(h, "123456"),
      bind: (token, h) => client.bindLoginHandle(token, h),
    });
    expect(result.accountId).toEqual(ACCOUNT);
    expect(seen.map((s) => `${s.method} ${s.url}`)).toEqual([
      "POST /api/auth/totp/recover",
      "PUT /api/auth/auth/login-handle",
    ]);
    expect(seen.some((s) => s.url.includes("discovery"))).toBe(false);
    expect(seen[1]?.authorization).toBe("Bearer sess");
    expect(seen[1]?.body).toEqual({ handle: target.handle.wire });
  });

  it("a failed re-bind does not undo the sign-in", async () => {
    stubFetch((url) =>
      url.endsWith("/recovery-contact/recover/finish")
        ? { status: 200, json: { account_id_b64: ACCOUNT_B64URL, session_token: "sess", session_expires_at_ms: 1 } }
        : { status: 503, json: { error: "login-by-name-disabled" } },
    );
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const client = new AuthClient("/api/auth");
    const result = await accessByName({
      target: nameTargetWith(wasm, "alice"),
      run: (h) => client.recoveryContactRecoverFinish(h, ContactKind.Email, "123456"),
      bind: (token, h) => client.bindLoginHandle(token, h),
    });
    expect(result.sessionToken).toBe("sess");
  });

  it("a refused sign-in never binds", async () => {
    const bind = vi.fn(async () => undefined);
    const err = await accessByName({
      target: nameTargetWith(wasm, "alice"),
      run: async () => {
        throw new AuthHttpError(401, "/login/finish", "");
      },
      bind,
    }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AuthHttpError);
    expect(bind).not.toHaveBeenCalled();
  });

  it("AuthContext's sign-in and recovery paths hold no discovery lookup", () => {
    const src = readFileSync(fileURLToPath(new URL("./AuthContext.tsx", import.meta.url)), "utf8");
    expect(src).not.toMatch(/discovery\.lookup\(/);
    const resolution = readFileSync(fileURLToPath(new URL("./recover-resolution.ts", import.meta.url)), "utf8");
    expect(resolution).not.toMatch(/lookup/);
  });
});

describe("the binding routes", () => {
  it("PUT and DELETE /auth/login-handle carry the session", async () => {
    const seen = stubFetch((_url, method) =>
      method === "PUT" ? { status: 200, json: { bound: true } } : { status: 200, json: { removed: true } },
    );
    const client = new AuthClient("/api/auth");
    const h = loginHandleWith(wasm, "alice").wire;
    await client.bindLoginHandle("tok", h);
    await expect(client.releaseLoginHandle("tok")).resolves.toBe(true);
    expect(seen.map((s) => `${s.method} ${s.url} ${s.authorization}`)).toEqual([
      "PUT /api/auth/auth/login-handle Bearer tok",
      "DELETE /api/auth/auth/login-handle Bearer tok",
    ]);
    expect(seen[0]?.body).toEqual({ handle: h });
    expect(seen[1]?.body).toBeNull();
  });

  it("409 handle-taken surfaces with its code", async () => {
    stubFetch(() => ({ status: 409, json: { error: "handle-taken" } }));
    const err = await new AuthClient("/api/auth")
      .bindLoginHandle("tok", loginHandleWith(wasm, "alice").wire)
      .catch((e: unknown) => e);
    expect((err as AuthHttpError).status).toBe(409);
    expect((err as AuthHttpError).errorCode).toBe("handle-taken");
  });
});

describe("claim order: auth first, then discovery", () => {
  function steps() {
    const order: string[] = [];
    return {
      order,
      bind: vi.fn(async () => {
        order.push("bind");
      }),
      register: vi.fn(async () => {
        order.push("register");
        return { normalisedUsername: "alice" };
      }),
      release: vi.fn(async () => {
        order.push("release");
      }),
    };
  }

  it("binds the handle before registering the name, and keeps it", async () => {
    const s = steps();
    await expect(claimUsername({ username: "alice", ...s })).resolves.toEqual({ normalisedUsername: "alice" });
    expect(s.order).toEqual(["bind", "register"]);
  });

  it("auth 409 stops before discovery, with the taken copy", async () => {
    const s = steps();
    s.bind.mockImplementation(async () => {
      s.order.push("bind");
      throw new AuthHttpError(409, "/auth/login-handle", JSON.stringify({ error: "handle-taken" }));
    });
    const err = await claimUsername({ username: "alice", ...s }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(UsernameTakenError);
    expect(signupErrorMessage(err)).toBe("That username is taken. Choose another one.");
    expect(s.register).not.toHaveBeenCalled();
    expect(s.release).not.toHaveBeenCalled();
  });

  it("discovery taken releases the handle, then shows the taken copy", async () => {
    const s = steps();
    s.register.mockImplementation(async () => {
      s.order.push("register");
      throw new DiscoveryHttpError(409, "/discovery/register", "taken");
    });
    const err = await claimUsername({ username: "alice", ...s }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(UsernameTakenError);
    expect(signupErrorMessage(err)).toBe("That username is taken. Choose another one.");
    expect(s.order).toEqual(["bind", "register", "release"]);
  });

  it("any other discovery failure also releases the handle, and rethrows as is", async () => {
    const s = steps();
    const failure = new DiscoveryHttpError(500, "/discovery/register", "boom");
    s.register.mockImplementation(async () => {
      s.order.push("register");
      throw failure;
    });
    await expect(claimUsername({ username: "alice", ...s })).rejects.toBe(failure);
    expect(s.order).toEqual(["bind", "register", "release"]);
  });

  it("another auth failure is rethrown and discovery is not asked", async () => {
    const s = steps();
    const failure = new AuthHttpError(503, "/auth/login-handle", JSON.stringify({ error: "login-by-name-disabled" }));
    s.bind.mockRejectedValue(failure);
    await expect(claimUsername({ username: "alice", ...s })).rejects.toBe(failure);
    expect(s.register).not.toHaveBeenCalled();
  });
});
