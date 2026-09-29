// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 — small rules the reach wiring rests on: the delegation header
 * name pinned against the core, the membership-shrink trigger, the setting's
 * stamp, and telling another leaf's Welcome from a failed join.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { DS_DELEGATION_HEADER } from "../ds/client";
import { isWelcomeForAnotherLeaf } from "../contacts/MailboxContext";
import { observeMembership } from "../profile/membership-baseline";
import { nextStamp } from "./visibility-store";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

function memoryStorage(): { getItem(k: string): string | null; setItem(k: string, v: string): void } {
  const m = new Map<string, string>();
  return { getItem: (k) => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v) };
}

describe("pins against the core", () => {
  it("the delegation header name is the core's", () => {
    expect(DS_DELEGATION_HEADER).toBe(wasm.delegationHeaderName());
  });
});

describe("the membership shrink trigger", () => {
  it("never on the first observation, on a removal, not on growth, not on a torn roster", () => {
    const s = memoryStorage();
    expect(observeMembership("acct", "g", new Set(["a", "b", "c"]), s)).toEqual({ grew: false, shrank: false });
    expect(observeMembership("acct", "g", new Set(["a", "b", "c", "d"]), s)).toEqual({ grew: true, shrank: false });
    expect(observeMembership("acct", "g", new Set(["a", "b", "d"]), s)).toEqual({ grew: false, shrank: true });
    expect(observeMembership("acct", "g", new Set(), s)).toEqual({ grew: false, shrank: false });
    // Remove + add in one batch: both.
    expect(observeMembership("acct", "g", new Set(["a", "b", "e"]), s)).toEqual({ grew: true, shrank: true });
  });
});

describe("the setting's stamp", () => {
  it("is strictly increasing even when the clock stands still or goes back", () => {
    expect(nextStamp(10n, 20)).toBe(20n);
    expect(nextStamp(20n, 20)).toBe(21n);
    expect(nextStamp(50n, 20)).toBe(51n);
  });
});

describe("a Welcome for another leaf of this account", () => {
  it("is recognised by the OpenMLS no-matching-key-package refusal only", () => {
    expect(isWelcomeForAnotherLeaf(new Error("group error: welcome -> group: NoMatchingKeyPackage"))).toBe(true);
    expect(isWelcomeForAnotherLeaf(new Error("staged welcome: InvalidSignature"))).toBe(false);
    expect(isWelcomeForAnotherLeaf("NoMatchingKeyPackage")).toBe(true);
  });
});
