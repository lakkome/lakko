// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { resolveRecoverUsername } from "./recover-resolution";

describe("resolveRecoverUsername (ADR-0054 S5: a name, never a looked-up id)", () => {
  const cached = { accountIdB64: "CACHED", username: "alice" };

  it("the typed name wins, trimmed", () => {
    expect(resolveRecoverUsername({ cached, username: "  bob  " })).toBe("bob");
  });

  it("an empty field falls back to the account cached on this device", () => {
    expect(resolveRecoverUsername({ cached, username: undefined })).toBe("alice");
    expect(resolveRecoverUsername({ cached, username: "   " })).toBe("alice");
  });

  it("a fresh device with no typed name asks for one", () => {
    expect(() => resolveRecoverUsername({ cached: null, username: "" })).toThrow(/username/);
  });

  it("a cached guest account has no name to address", () => {
    expect(() =>
      resolveRecoverUsername({ cached: { accountIdB64: "G", username: "" }, username: undefined }),
    ).toThrow(/username/);
  });
});
