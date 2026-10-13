// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** Log out at the server too (`logout-owed.ts`, ADR-0074 section 2.1). */
import { describe, expect, it } from "vitest";

import {
  clearLogoutOwed,
  isLogoutOwed,
  logoutAttempt,
  logoutPlan,
  markLogoutOwed,
  runServerLogout,
  type ServerLogoutPorts,
} from "./logout-owed";
import type { MarkerStorage } from "./phrase-gate";
import type { SessionPair } from "./session-pair";
import { decideSilentRestore } from "./silent-restore";

const NOW = 1_800_000_000_000;

const PAIR: SessionPair = {
  sessionToken: "v3.acct.1.dev.iat.exp.tag",
  sessionExpiresAtMs: NOW + 60_000,
  refreshToken: "r2.acct.1.dev.iat.exp.tag",
  refreshExpiresAtMs: NOW + 1_000_000,
  keyGeneration: 1n,
};

function memory(): MarkerStorage {
  const rows = new Map<string, string>();
  return {
    getItem: (k) => rows.get(k) ?? null,
    setItem: (k, v) => void rows.set(k, v),
    removeItem: (k) => void rows.delete(k),
  };
}

function ports(over: Partial<ServerLogoutPorts> = {}): ServerLogoutPorts & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    now: () => NOW,
    refresh: async (rt) => {
      calls.push(`refresh ${rt}`);
      return { token: "fresh-token" };
    },
    logout: async (token) => {
      calls.push(`logout ${token}`);
      return 204;
    },
    ...over,
  };
}

describe("the owed marker", () => {
  it("is per account and cleared on its own", () => {
    const s = memory();
    markLogoutOwed(s, "a");
    expect(isLogoutOwed(s, "a")).toBe(true);
    expect(isLogoutOwed(s, "b")).toBe(false);
    clearLogoutOwed(s, "a");
    expect(isLogoutOwed(s, "a")).toBe(false);
    expect(isLogoutOwed(undefined, "a")).toBe(false);
  });

  it("keeps a reload from restoring a session the person logged out of", () => {
    const probe = {
      cachedAccount: { accountIdB64: "a", username: "alice" },
      deviceKeyPresent: true,
      sessionPair: PAIR,
      identitySeedPresent: true,
      nowMs: NOW,
    };
    expect(decideSilentRestore(probe).kind).toBe("restore");
    expect(decideSilentRestore({ ...probe, logoutOwed: true })).toEqual({
      kind: "anon-cached",
      username: "alice",
    });
  });
});

describe("logoutPlan", () => {
  it("uses the access token while it is valid", () => {
    expect(logoutPlan(PAIR, NOW)).toBe("use-token");
  });
  it("refreshes first once the access token expired", () => {
    expect(logoutPlan(PAIR, NOW + 60_000)).toBe("refresh-first");
  });
  it("has nothing to sign out without a pair or with a dead refresh credential", () => {
    expect(logoutPlan(null, NOW)).toBe("nothing");
    expect(logoutPlan(PAIR, NOW + 1_000_000)).toBe("nothing");
  });
});

describe("logoutAttempt", () => {
  it("is done when the server holds the floor or already refuses the credential", () => {
    expect(logoutAttempt(204)).toBe("done");
    expect(logoutAttempt(401)).toBe("done");
  });
  it("stays owed without an answer or without a verdict", () => {
    expect(logoutAttempt(null)).toBe("owed");
    expect(logoutAttempt(500)).toBe("owed");
    expect(logoutAttempt(503)).toBe("owed");
    expect(logoutAttempt(429)).toBe("owed");
  });
});

describe("runServerLogout", () => {
  it("logs out with the held token", async () => {
    const p = ports();
    expect(await runServerLogout(PAIR, p)).toBe("done");
    expect(p.calls).toEqual([`logout ${PAIR.sessionToken}`]);
  });

  it("refreshes an expired token and logs out with the new one", async () => {
    const p = ports({ now: () => NOW + 60_000 });
    expect(await runServerLogout(PAIR, p)).toBe("done");
    expect(p.calls).toEqual([`refresh ${PAIR.refreshToken}`, "logout fresh-token"]);
  });

  it("is done when the refresh is refused: no credential of this device works any more", async () => {
    const p = ports({ now: () => NOW + 60_000, refresh: async () => ({ status: 401 }) });
    expect(await runServerLogout(PAIR, p)).toBe("done");
    expect(p.calls).toEqual([]);
  });

  it("stays owed offline, at either request", async () => {
    expect(await runServerLogout(PAIR, ports({ logout: async () => null }))).toBe("owed");
    expect(
      await runServerLogout(
        PAIR,
        ports({ now: () => NOW + 60_000, refresh: async () => ({ status: null }) }),
      ),
    ).toBe("owed");
  });

  it("sends nothing when nothing is held", async () => {
    const p = ports();
    expect(await runServerLogout(null, p)).toBe("done");
    expect(p.calls).toEqual([]);
  });
});
