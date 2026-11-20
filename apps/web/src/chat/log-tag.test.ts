// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0091 phase 1b - a log tag never carries an identifier. */
import { describe, expect, it } from "vitest";

import { errorLogTag } from "./log-tag";
import { DsHttpError } from "../ds/client";
import { parseRegisteredCommits } from "../ds/client";

describe("errorLogTag", () => {
  it("keeps the names and the DS status, never the endpoint or the body", () => {
    const hex = "c14ae3b45bdb6a66".padEnd(64, "0");
    const ds = new DsHttpError(401, `/ds/group/${hex}/messages`, "proof required");
    const wrapped = Object.assign(new Error(`retire of ${hex} failed`), { name: "ContactsError", cause: ds });
    const tag = errorLogTag(wrapped);
    expect(tag).toBe("ContactsError>DsHttpError status=401");
    expect(tag).not.toContain(hex);
    expect(tag).not.toContain("proof");
  });

  it("names a typed refusal and survives odd values", () => {
    const refused = Object.assign(new Error("x"), { refusalReason: "notOwner" });
    expect(errorLogTag(refused)).toBe("Error refusal=notOwner");
    expect(errorLogTag("a string with an id 12345")).toBe("string");
    expect(errorLogTag(undefined)).toBe("unknown");
    expect(errorLogTag({ name: "has spaces and 0123456789abcdef" })).toBe("Error");
  });
});

describe("parseRegisteredCommits", () => {
  it("reads the DS `commits` field keyed by epoch", () => {
    const parsed = parseRegisteredCommits([
      { epoch: 7, id: 7967 },
      { epoch: 8, id: 8001 },
    ]);
    expect(parsed?.get(7n)).toBe(7967);
    expect(parsed?.get(8n)).toBe(8001);
  });

  it("is absent without the field and drops malformed entries", () => {
    expect(parseRegisteredCommits(undefined)).toBeUndefined();
    expect(parseRegisteredCommits({})).toBeUndefined();
    const parsed = parseRegisteredCommits([{ epoch: -1, id: 3 }, { epoch: 2 }, null, { epoch: 3, id: 9 }]);
    expect([...(parsed ?? new Map()).entries()]).toEqual([[3n, 9]]);
  });
});
