// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { LinkRecord, LinkRecordKind, SyncedState } from "../../../wasm/lakko_ffi_wasm";
import { effectiveLimit, isLinkLive, listedContactLinks, readLinkRecords, type LinkRecordModel } from "./link-records";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function model(over: Partial<LinkRecordModel>): LinkRecordModel {
  return {
    linkIdHex: "01".repeat(16),
    linkId: new Uint8Array(16).fill(1),
    linkSecret: new Uint8Array(12).fill(2),
    kind: "contact",
    ledgerId: new Uint8Array(6).fill(3),
    revocationSeed: new Uint8Array(32).fill(4),
    expiryMs: 0,
    maxUses: 0,
    uses: 0,
    revoked: false,
    mintedAtMs: 10,
    label: "",
    ...over,
  };
}

describe("link records (ADR-0079 section 4)", () => {
  it("lists live contact links newest first; used-up and revoked ones leave", () => {
    const shown = listedContactLinks(
      [
        model({ linkIdHex: "a", mintedAtMs: 1 }),
        model({ linkIdHex: "b", mintedAtMs: 3 }),
        model({ linkIdHex: "used", uses: 1 }),
        model({ linkIdHex: "gone", revoked: true }),
        model({ linkIdHex: "call", kind: "call" }),
        model({ linkIdHex: "old", expiryMs: 5 }),
      ],
      100,
    );
    expect(shown.map((r) => r.linkIdHex)).toEqual(["b", "a"]);
  });

  it("follows the capability's use-limit sentinels", () => {
    expect(effectiveLimit(0)).toBe(1);
    expect(effectiveLimit(10)).toBe(10);
    expect(effectiveLimit(0xffff_ffff)).toBeNull();
    expect(isLinkLive(model({ maxUses: 10, uses: 9 }), 0)).toBe(true);
    expect(isLinkLive(model({ maxUses: 10, uses: 10 }), 0)).toBe(false);
  });

  it("decides a request's link answer through the one Rust rule", () => {
    const s = new SyncedState();
    const record = new LinkRecord(
      new Uint8Array(16).fill(7),
      new Uint8Array(12).fill(8),
      LinkRecordKind.Contact,
      new Uint8Array(6).fill(9),
      new Uint8Array(32).fill(0x42),
      0n,
      0,
      1n,
      "home",
    );
    expect(s.setLinkRecord(5n, record)).toBe(true);
    const id = new Uint8Array(16).fill(7);
    const bob = new Uint8Array(32).fill(0xb0);
    expect(s.linkAnswerVerdict(id, new Uint8Array(12).fill(8), bob, 0n)).toBe("accept");
    // A secret that differs in one byte never accepts.
    const wrong = new Uint8Array(12).fill(8);
    wrong[11] = 0;
    expect(s.linkAnswerVerdict(id, wrong, bob, 0n)).toBe("dead");
    expect(s.recordLinkUse(6n, id, bob)).toBe(true);
    expect(s.linkAnswerVerdict(id, new Uint8Array(12).fill(8), bob, 0n)).toBe("answered");
    expect(s.linkAnswerVerdict(id, new Uint8Array(12).fill(8), new Uint8Array(32).fill(1), 0n)).toBe("dead");
    const read = readLinkRecords(s, LinkRecordKind.Contact);
    expect(read).toHaveLength(1);
    expect(read[0]?.uses).toBe(1);
    expect(read[0]?.label).toBe("home");
    // A revocation is final: a later live write from another device loses.
    const other = new SyncedState();
    const envelope = s.linkRecordEnvelopeFor(id) as Uint8Array;
    expect(other.applyLinkRecordEnvelope(envelope)).toBe(true);
    expect(other.revokeLinkRecord(7n, id)).toBe(true);
    expect(s.recordLinkUse(100n, id, new Uint8Array(32).fill(2))).toBe(true);
    expect(other.applyLinkRecordEnvelope(s.linkRecordEnvelopeFor(id) as Uint8Array)).toBe(false);
    expect(readLinkRecords(other, LinkRecordKind.Contact)[0]?.revoked).toBe(true);
    // The device that revoked it holds the seed that deletes the ledger row
    // (reworked 2026-10-17): the record carried it there.
    expect(readLinkRecords(other, LinkRecordKind.Contact)[0]?.revocationSeed).toEqual(new Uint8Array(32).fill(0x42));
    record.free();
    s.free();
    other.free();
  });
});
