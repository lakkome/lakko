// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The reload restore decision (`silent-restore.ts`): which state each
 * combination of stored secrets lands in. The load-bearing property is that
 * there is no half state — a restore needs every piece, and anything less is
 * `anon`. Since ADR-0073 the piece that must be live is the refresh
 * credential: an expired access token beside a live credential restores.
 */
import { describe, expect, it } from "vitest";

import { decideSilentRestore, type StoredSecretsProbe } from "./silent-restore";
import type { SessionPair } from "./session-pair";

const NOW = 1_800_000_000_000;

const PAIR: SessionPair = {
  sessionToken: "v2.acct.1.exp.tag",
  sessionExpiresAtMs: NOW + 60_000,
  refreshToken: "r1.acct.1.exp.tag",
  refreshExpiresAtMs: NOW + 30 * 24 * 60 * 60 * 1000,
  keyGeneration: 1n,
};

function probe(overrides: Partial<StoredSecretsProbe> = {}): StoredSecretsProbe {
  return {
    cachedAccount: { accountIdB64: "QUJDRA==", username: "alice" },
    deviceKeyPresent: true,
    sessionPair: PAIR,
    identitySeedPresent: true,
    nowMs: NOW,
    ...overrides,
  };
}

describe("decideSilentRestore", () => {
  it("restores when the account marker, device key, live credential and seed are all present", () => {
    expect(decideSilentRestore(probe())).toEqual({
      kind: "restore",
      accountIdB64: "QUJDRA==",
      username: "alice",
      sessionPair: PAIR,
    });
  });

  it("restores on an expired access token while the refresh credential is live", () => {
    const decision = decideSilentRestore(
      probe({ sessionPair: { ...PAIR, sessionExpiresAtMs: NOW - 1 } }),
    );
    expect(decision.kind).toBe("restore");
  });

  it("is plain anon on a browser that never held an account", () => {
    expect(
      decideSilentRestore(
        probe({
          cachedAccount: null,
          deviceKeyPresent: false,
          sessionPair: null,
          identitySeedPresent: false,
        }),
      ),
    ).toEqual({ kind: "anon" });
  });

  it("is plain anon without a marker even if stray secrets remain", () => {
    expect(decideSilentRestore(probe({ cachedAccount: null }))).toEqual({
      kind: "anon",
    });
  });

  const fallbacks: Array<[string, Partial<StoredSecretsProbe>]> = [
    ["no device key", { deviceKeyPresent: false }],
    ["no session pair (logged out)", { sessionPair: null }],
    [
      "an expired refresh credential",
      { sessionPair: { ...PAIR, refreshExpiresAtMs: NOW } },
    ],
    ["no identity seed", { identitySeedPresent: false }],
    [
      "nothing but the marker",
      { deviceKeyPresent: false, sessionPair: null, identitySeedPresent: false },
    ],
  ];
  for (const [label, overrides] of fallbacks) {
    it(`lands in anon with the cached username on ${label}`, () => {
      expect(decideSilentRestore(probe(overrides))).toEqual({
        kind: "anon-cached",
        username: "alice",
      });
    });
  }
});
