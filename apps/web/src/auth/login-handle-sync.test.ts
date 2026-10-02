// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 S5 — a session that landed on any path binds its username's
 * login handle once, best-effort, and a failure changes nothing for the
 * user.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { AuthClient } from "./client";
import { loginHandleWith } from "./login-handle";
import { createLoginHandleSync, type LoginHandleSync } from "./login-handle-sync";
import { decideSilentRestore } from "./silent-restore";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

interface Seen {
  readonly url: string;
  readonly method: string;
  readonly authorization: string | undefined;
  readonly body: Record<string, unknown> | null;
}

function stubFetch(status: number, json: unknown): Seen[] {
  const seen: Seen[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const headers = (init?.headers ?? {}) as Record<string, string>;
      seen.push({
        url: String(input),
        method: init?.method ?? "GET",
        authorization: headers["authorization"],
        body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null,
      });
      return new Response(JSON.stringify(json), { status });
    }),
  );
  return seen;
}

/** The sync as `AuthContext` builds it, over a real `AuthClient`. */
function appSync(warn = vi.fn()): { sync: LoginHandleSync; warn: typeof warn } {
  const client = new AuthClient("/api/auth");
  const sync = createLoginHandleSync({
    handleFor: async (username) => loginHandleWith(wasm, username),
    bind: (token, handle) => client.bindLoginHandle(token, handle),
    warn,
  });
  return { sync, warn };
}

const NOW = 1_800_000_000_000;

describe("a session binds its username's login handle", () => {
  it("the reload restore binds once, with the restored session and the canonical name's handle", async () => {
    const decision = decideSilentRestore({
      cachedAccount: { accountIdB64: "QUJDRA==", username: "Alice" },
      deviceKeyPresent: true,
      sessionToken: { token: "v1.restored", expiresAtMs: NOW + 60_000 },
      identitySeedPresent: true,
      nowMs: NOW,
    });
    expect(decision.kind).toBe("restore");
    if (decision.kind !== "restore") return;

    const seen = stubFetch(200, { bound: true });
    const { sync } = appSync();
    await expect(sync.ensure(decision.sessionToken, decision.username)).resolves.toBe("bound");

    expect(seen.map((s) => `${s.method} ${s.url} ${s.authorization}`)).toEqual([
      "PUT /api/auth/auth/login-handle Bearer v1.restored",
    ]);
    expect(seen[0]?.body).toEqual({ handle: loginHandleWith(wasm, "alice").wire });
  });

  it("no username (a guest) binds nothing", async () => {
    const seen = stubFetch(200, { bound: true });
    const { sync } = appSync();
    await expect(sync.ensure("v1.guest", "")).resolves.toBe("no-username");
    await expect(sync.ensure("v1.guest", "   ")).resolves.toBe("no-username");
    expect(seen).toHaveLength(0);
  });

  it("a name the username rule refuses binds nothing and logs nothing", async () => {
    const seen = stubFetch(200, { bound: true });
    const { sync, warn } = appSync();
    await expect(sync.ensure("v1.tok", "a")).resolves.toBe("no-username");
    expect(seen).toHaveLength(0);
    expect(warn).not.toHaveBeenCalled();
  });

  it("409 handle-taken is logged by status only and never throws", async () => {
    stubFetch(409, { error: "handle-taken" });
    const { sync, warn } = appSync();
    await expect(sync.ensure("v1.secret-token", "alice")).resolves.toBe("failed");
    expect(warn).toHaveBeenCalledTimes(1);
    const [, detail] = warn.mock.calls[0] ?? [];
    expect(detail).toBe(409);
    const logged = JSON.stringify(warn.mock.calls);
    expect(logged).not.toContain("v1.secret-token");
    expect(logged).not.toContain(loginHandleWith(wasm, "alice").wire);
  });

  it("503 login-by-name-disabled is silent", async () => {
    stubFetch(503, { error: "login-by-name-disabled" });
    const { sync, warn } = appSync();
    await expect(sync.ensure("v1.tok", "alice")).resolves.toBe("disabled");
    expect(warn).not.toHaveBeenCalled();
  });

  it("a network error is logged and swallowed", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("offline");
      }),
    );
    const { sync, warn } = appSync();
    await expect(sync.ensure("v1.tok", "alice")).resolves.toBe("failed");
    expect(warn).toHaveBeenCalledTimes(1);
  });

  it("repeated installs of the same session bind once, racing ones included", async () => {
    const seen = stubFetch(200, { bound: true });
    const { sync } = appSync();
    const first = await Promise.all([sync.ensure("v1.tok", "alice"), sync.ensure("v1.tok", "@Alice")]);
    expect(first.sort()).toEqual(["already", "bound"]);
    await expect(sync.ensure("v1.tok", "alice")).resolves.toBe("already");
    expect(seen).toHaveLength(1);
  });

  it("a failed bind is not retried within the same session", async () => {
    const seen = stubFetch(409, { error: "handle-taken" });
    const { sync } = appSync();
    await sync.ensure("v1.tok", "alice");
    await expect(sync.ensure("v1.tok", "alice")).resolves.toBe("already");
    expect(seen).toHaveLength(1);
  });

  it("a new session, or a new username on the same session, binds again", async () => {
    const seen = stubFetch(200, { bound: true });
    const { sync } = appSync();
    await sync.ensure("v1.one", "alice");
    await sync.ensure("v1.two", "alice");
    await sync.ensure("v1.two", "bob");
    expect(seen.map((s) => s.authorization)).toEqual(["Bearer v1.one", "Bearer v1.two", "Bearer v1.two"]);
  });

  it("a path that already bound the handle (sign-in by name, signup) is not repeated", async () => {
    const seen = stubFetch(200, { bound: true });
    const { sync } = appSync();
    sync.markBound("v1.tok", loginHandleWith(wasm, "alice").wire);
    await expect(sync.ensure("v1.tok", "alice")).resolves.toBe("already");
    expect(seen).toHaveLength(0);
  });
});

describe("AuthContext wiring", () => {
  const src = readFileSync(fileURLToPath(new URL("./AuthContext.tsx", import.meta.url)), "utf8");

  it("binds on every session install, from the live session state", () => {
    expect(src).toMatch(
      /createEffect\(\(\) => \{\s*const s = state\(\);\s*const secrets = identitySecrets\(\);\s*if \(s\.kind !== "session" \|\| secrets === null\) return;\s*void loginHandleSync\.ensure\(secrets\.sessionToken, s\.account\.username\);/,
    );
  });

  it("every sign-in by name records its re-bind", () => {
    // The one remaining direct closure is the sync's own bind.
    expect(src.match(/bind: \(token, handle\) => client\.bindLoginHandle/g)?.length).toBe(1);
    // Password login, phrase recovery, the authenticator code, the
    // password replacement from a device that holds the keys
    // (docs/ux/identity-recovery.md 7.1) and the sign-in after the local
    // three-word check.
    expect(src.match(/bind: bindByName,/g)?.length).toBe(5);
  });
});
