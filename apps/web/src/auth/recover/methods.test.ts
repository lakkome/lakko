// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { RECOVER_METHODS, RESTORE_IDENTITY_METHODS, recoverMethod } from "./methods";

const BANNED = /OPAQUE|PAKE|OPRF|MLS|KeyPackage|epoch|pseudonym|sprint|\b1\.\d+[a-z]?\b/i;

describe("the recovery method rows", () => {
  it("are in the order the spec lists them", () => {
    expect(RECOVER_METHODS.map((m) => m.title)).toEqual([
      "Recovery phrase",
      "Another device",
      "Three words",
      "Authenticator",
    ]);
  });

  it("carry no protocol words or sprint numbers", () => {
    for (const m of [...RECOVER_METHODS, ...RESTORE_IDENTITY_METHODS]) {
      expect(m.title).not.toMatch(BANNED);
      expect(m.sub).not.toMatch(BANNED);
    }
  });

  it("resolve by id", () => {
    expect(recoverMethod("totp").title).toBe("Authenticator");
  });
});
