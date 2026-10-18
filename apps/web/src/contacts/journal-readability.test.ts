// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 (order of 2026-10-17) - the journal readability rule as the
 * DeviceGroup receive loop applies it, through the real core
 * (`JournalEpochs`). Rows are stand-ins whose framed epochs the test names.
 */

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { JournalEpochs } from "../../wasm/lakko_ffi_wasm";
import { journalReadability } from "./journal-readability";
import { linkClaimRecord } from "./link-claims";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function memoryStorage(): Pick<Storage, "getItem" | "setItem" | "removeItem"> {
  const m = new Map<string, string>();
  return {
    getItem: (k) => m.get(k) ?? null,
    setItem: (k, v) => {
      m.set(k, v);
    },
    removeItem: (k) => {
      m.delete(k);
    },
  };
}

/** A stand-in row: byte 0 = sealed epoch, byte 1 = 1 for a commit. */
const row = (sealed: number, commit = false, tag = 0): Uint8Array => Uint8Array.from([sealed, commit ? 1 : 0, tag]);
const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

function loop(record: ReturnType<typeof linkClaimRecord>, groupEpoch: () => bigint) {
  return journalReadability({
    record,
    groupIdHex: "aa",
    epochOf: (m) => BigInt(m[0] ?? 0),
    epochAfter: (m) => BigInt((m[0] ?? 0) + (m[1] === 1 ? 1 : 0)),
    groupEpoch,
    newEpochs: (base) => new JournalEpochs(base),
    fromParts: (base, ids, afters) => JournalEpochs.fromParts(base, ids, afters),
    hex,
  });
}

describe("the journal readability rule in the receive loop", () => {
  it("a claim straddled by another device's commit does not count for its author", () => {
    const storage = memoryStorage();
    const record = linkClaimRecord("acct", storage);
    const straddled = row(5, false, 1);
    const fresh = row(6, false, 2);
    record.addOwnWire("k", hex(straddled));
    record.addOwnWire("k", hex(fresh));
    let epoch = 5n;
    const r = loop(record, () => epoch);
    // Another device's commit at 5 lands at row 11; this device applies it.
    epoch = 6n;
    r.observe(11, row(5, true));
    expect(r.judgeOwn(12, straddled)).toBe("claim");
    expect(r.judgeOwn(13, fresh)).toBe("claim");
    r.dispose();
    expect(record.own("k")?.ids).toEqual([13]);
  });

  it("an own commit merged before the loop reaches the claim does not straddle it", () => {
    const record = linkClaimRecord("acct", memoryStorage());
    const claim = row(5, false, 1);
    record.addOwnWire("k", hex(claim));
    // Persisted from an earlier pass: the journal was at 5.
    record.setJournal({ groupIdHex: "aa", base: "5", commitRowIds: [], commitEpochsAfter: [] });
    // The group is already at 6: the device merged its own commit locally.
    const r = loop(record, () => 6n);
    expect(r.judgeOwn(20, claim)).toBe("claim");
    r.observe(21, row(5, true)); // its own commit's row, skipped as an echo
    r.dispose();
    expect(record.own("k")?.ids).toEqual([20]);
    expect(record.journal("aa")?.commitRowIds).toEqual(["21"]);
  });

  it("a straddled note is not confirmed; one at the journal's epoch is", () => {
    const record = linkClaimRecord("acct", memoryStorage());
    record.setJournal({ groupIdHex: "aa", base: "7", commitRowIds: [], commitEpochsAfter: [] });
    const r = loop(record, () => 8n);
    r.observe(30, row(7, true));
    record.setNoteWire("k", hex(row(7, false, 9)));
    expect(r.judgeOwn(31, row(7, false, 9))).toBe("note");
    expect(record.own("k")?.noteConfirmed).toBe(false);
    record.setNoteWire("k", hex(row(8, false, 9)));
    expect(r.judgeOwn(32, row(8, false, 9))).toBe("note");
    expect(record.own("k")?.noteConfirmed).toBe(true);
    r.dispose();
  });

  it("a fresh record is kept at once, so a later batch does not restart it at the group's epoch", () => {
    const storage = memoryStorage();
    const record = linkClaimRecord("acct", storage);
    loop(record, () => 5n); // a batch that never reaches dispose
    expect(linkClaimRecord("acct", storage).journal("aa")?.base).toBe("5");
    // The device merges its own commit (group at 6) before the next batch.
    const claim = row(5, false, 1);
    record.addOwnWire("k", hex(claim));
    const next = loop(linkClaimRecord("acct", storage), () => 6n);
    expect(next.judgeOwn(40, claim)).toBe("claim");
    next.dispose();
    expect(linkClaimRecord("acct", storage).own("k")?.ids).toEqual([40]);
  });

  it("a refused commit does not move the journal, and the answers survive a restart", () => {
    const storage = memoryStorage();
    const record = linkClaimRecord("acct", storage);
    record.setJournal({ groupIdHex: "aa", base: "3", commitRowIds: [], commitEpochsAfter: [] });
    const r = loop(record, () => 3n);
    r.observe(5, row(3, true)); // every device refused it: the group stayed at 3
    r.dispose();
    const again = loop(linkClaimRecord("acct", storage), () => 4n);
    linkClaimRecord("acct", storage).addOwnWire("k", hex(row(3, false, 1)));
    expect(again.judgeOwn(6, row(3, false, 1))).toBe("claim");
    again.dispose();
    expect(linkClaimRecord("acct", storage).own("k")?.ids).toEqual([6]);
  });
});
