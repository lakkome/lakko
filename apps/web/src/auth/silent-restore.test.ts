// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The reload restore decision (`silent-restore.ts`): which state each
 * combination of stored secrets lands in. The load-bearing property is that
 * there is no half state — a restore needs every piece, and anything less is
 * `anon`.
 */
import { describe, expect, it } from "vitest";

import { decideSilentRestore, type StoredSecretsProbe } from "./silent-restore";

const NOW = 1_800_000_000_000;

function probe(overrides: Partial<StoredSecretsProbe> = {}): StoredSecretsProbe {
  return {
    cachedAccount: { accountIdB64: "QUJDRA==", username: "alice" },
    deviceKeyPresent: true,
    sessionToken: { token: "v1.tok", expiresAtMs: NOW + 60_000 },
    identitySeedPresent: true,
    nowMs: NOW,
    ...overrides,
  };
}

describe("decideSilentRestore", () => {
  it("restores when the account marker, device key, live token and seed are all present", () => {
    expect(decideSilentRestore(probe())).toEqual({
      kind: "restore",
      accountIdB64: "QUJDRA==",
      username: "alice",
      sessionToken: "v1.tok",
      sessionExpiresAtMs: NOW + 60_000,
    });
  });

  it("is plain anon on a browser that never held an account", () => {
    expect(
      decideSilentRestore(
        probe({
          cachedAccount: null,
          deviceKeyPresent: false,
          sessionToken: null,
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
    ["no session token (logged out)", { sessionToken: null }],
    [
      "an expired session token",
      { sessionToken: { token: "v1.tok", expiresAtMs: NOW } },
    ],
    ["no identity seed", { identitySeedPresent: false }],
    [
      "nothing but the marker",
      { deviceKeyPresent: false, sessionToken: null, identitySeedPresent: false },
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
