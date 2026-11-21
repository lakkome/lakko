// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 (security review 2026-11-21) - the destructive steps follow the
 * core's `deletionEffects`, against the REAL core rule.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  deletionEffectsOf,
  withAnswer,
  withStatement,
  type AccountStatusRules,
  type PersonStatusRecord,
} from "./account-status";
import { peopleToRemoveFromGroups, pendingHeldToDrop } from "./deletion-actions";

let rules: AccountStatusRules;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
  rules = wasm as unknown as AccountStatusRules;
});

const NOW = 1_800_000_000_000;

function records(): Map<string, PersonStatusRecord> {
  return new Map<string, PersonStatusRecord>([
    ["statement-only", withStatement(undefined)],
    ["absent-only", withAnswer(undefined, "absent", NOW)],
    ["both", withAnswer(withStatement(undefined), "absent", NOW)],
    ["reverted", withAnswer(withAnswer(withStatement(undefined), "absent", NOW), "same-person", NOW + 1)],
    ["stale-absent", withStatement(withAnswer(undefined, "absent", NOW - 1))],
  ]);
}

describe("the destructive steps", () => {
  it("remove from groups only on a statement AND an Absent answer after it", () => {
    const recs = records();
    const effectsOf = (k: string) => deletionEffectsOf(rules, recs.get(k));
    expect(peopleToRemoveFromGroups(recs.keys(), effectsOf)).toEqual(["both"]);
  });

  it("drop held sends only on the same evidence, once per pending contact", () => {
    const recs = records();
    const effectsOf = (k: string) => deletionEffectsOf(rules, recs.get(k));
    const pending = [...recs.keys()].map((k, i) => ({ accountIdHex: `${i}`.padStart(32, "0"), signaturePubkeyB64: k }));
    expect(pendingHeldToDrop(pending, effectsOf, new Set()).map((p) => p.signaturePubkeyB64)).toEqual(["both"]);
    const both = pending.find((p) => p.signaturePubkeyB64 === "both");
    expect(pendingHeldToDrop(pending, effectsOf, new Set([both?.accountIdHex ?? ""]))).toEqual([]);
  });

  it("a SamePerson answer after the statement shows the person again and does nothing destructive", () => {
    const e = deletionEffectsOf(rules, records().get("reverted"));
    expect(e).toEqual({ showDeleted: false, removeFromGroups: false, dropHeldSends: false });
  });
});
