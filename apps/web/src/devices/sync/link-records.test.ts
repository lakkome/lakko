// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { groupLinkRecordId, LinkRecord, LinkRecordKind, SyncedState } from "../../../wasm/lakko_ffi_wasm";
import {
  effectiveLimit,
  isLinkLive,
  kindOfNumber,
  listedContactLinks,
  listedTargetLinks,
  numberOfKind,
  readLinkRecords,
  type LinkRecordModel,
} from "./link-records";

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
    shared: false,
    editable: true,
    mintedAtMs: 10,
    label: "",
    targetId: new Uint8Array(),
    inviteSeed: new Uint8Array(),
    admissionVersion: 0,
    capability: new Uint8Array(),
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

  it("judges expiry at the request's DS stamp and a revocation now (ADR-0079, 2026-10-19)", () => {
    const s = new SyncedState();
    const id = new Uint8Array(16).fill(3);
    const secret = new Uint8Array(12).fill(4);
    const record = new LinkRecord(id, secret, LinkRecordKind.Contact, new Uint8Array(6).fill(6), new Uint8Array(32).fill(0x11), 1_000n, 5, 1n, "");
    expect(s.setLinkRecord(5n, record)).toBe(true);
    const bob = new Uint8Array(32).fill(0xb0);
    // Add pressed before the expiry; this device online long after it: accepted.
    expect(s.linkAnswerVerdict(id, secret, bob, 999n)).toBe("accept");
    // Add pressed at or after the expiry: dead (refused).
    expect(s.linkAnswerVerdict(id, secret, bob, 1_000n)).toBe("dead");
    // Revoked before this account answered: dead even for an earlier Add.
    expect(s.revokeLinkRecord(6n, id)).toBe(true);
    expect(s.linkAnswerVerdict(id, secret, bob, 1n)).toBe("dead");
    // A link this device holds no record of: unknown, never refused.
    expect(s.linkAnswerVerdict(new Uint8Array(16).fill(9), secret, bob, 1n)).toBe("unknown");
    record.free();
    s.free();
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
    // UX section 9: editable until copied or used, and a copy is final.
    expect(readLinkRecords(s, LinkRecordKind)[0]?.editable).toBe(true);
    // A secret that differs in one byte never accepts.
    const wrong = new Uint8Array(12).fill(8);
    wrong[11] = 0;
    expect(s.linkAnswerVerdict(id, wrong, bob, 0n)).toBe("dead");
    expect(s.recordLinkUse(6n, id, bob)).toBe(true);
    expect(s.linkAnswerVerdict(id, new Uint8Array(12).fill(8), bob, 0n)).toBe("answered");
    expect(s.linkAnswerVerdict(id, new Uint8Array(12).fill(8), new Uint8Array(32).fill(1), 0n)).toBe("dead");
    const read = readLinkRecords(s, LinkRecordKind);
    expect(read).toHaveLength(1);
    expect(read[0]?.uses).toBe(1);
    expect(read[0]?.label).toBe("home");
    expect(read[0]?.editable).toBe(false);
    expect(read[0]?.shared).toBe(false);
    expect(s.markLinkShared(7n, id)).toBe(true);
    expect(s.markLinkShared(8n, id)).toBe(false);
    expect(readLinkRecords(s, LinkRecordKind)[0]?.shared).toBe(true);
    // A revocation is final: a later live write from another device loses.
    const other = new SyncedState();
    const envelope = s.linkRecordEnvelopeFor(id) as Uint8Array;
    expect(other.applyLinkRecordEnvelope(envelope)).toBe(true);
    expect(other.revokeLinkRecord(7n, id)).toBe(true);
    expect(s.recordLinkUse(100n, id, new Uint8Array(32).fill(2))).toBe(true);
    expect(other.applyLinkRecordEnvelope(s.linkRecordEnvelopeFor(id) as Uint8Array)).toBe(false);
    expect(readLinkRecords(other, LinkRecordKind)[0]?.revoked).toBe(true);
    // The device that revoked it holds the seed that deletes the ledger row
    // (reworked 2026-10-17): the record carried it there.
    expect(readLinkRecords(other, LinkRecordKind)[0]?.revocationSeed).toEqual(new Uint8Array(32).fill(0x42));
    record.free();
    s.free();
    other.free();
  });

  it("maps every kind both ways", () => {
    for (const kind of ["contact", "call", "group", "channel", "publisher"] as const) {
      expect(kindOfNumber(numberOfKind(kind, LinkRecordKind), LinkRecordKind)).toBe(kind);
    }
    expect(kindOfNumber(99, LinkRecordKind)).toBeNull();
  });

  it("keeps a channel link's target, invite seed and capability through the register (ADR-0087 phase B)", () => {
    const ledgerId = new Uint8Array(6).fill(7);
    const channelId = new Uint8Array(16).fill(9);
    const record = new LinkRecord(
      groupLinkRecordId(ledgerId),
      new Uint8Array(12).fill(2),
      LinkRecordKind.Channel,
      ledgerId,
      new Uint8Array(32).fill(0x11),
      0n,
      0xffffffff,
      5n,
      "Site",
      channelId,
      new Uint8Array(32).fill(0x22),
      3,
      new Uint8Array(40).fill(0x33),
    );
    const s = new SyncedState();
    s.setLinkRecord(1n, record);
    const [read] = readLinkRecords(s, LinkRecordKind);
    expect(read?.kind).toBe("channel");
    expect(read?.targetId).toEqual(channelId);
    expect(read?.inviteSeed).toEqual(new Uint8Array(32).fill(0x22));
    expect(read?.admissionVersion).toBe(3);
    expect(read?.capability).toEqual(new Uint8Array(40).fill(0x33));
    expect(read?.label).toBe("Site");
    expect(listedTargetLinks(read === undefined ? [] : [read], "channel", "09".repeat(16), 10).length).toBe(1);
    expect(listedTargetLinks(read === undefined ? [] : [read], "group", "09".repeat(16), 10).length).toBe(0);
  });

  it("lists only live links of one target", () => {
    const target = new Uint8Array(32).fill(5);
    const out = listedTargetLinks(
      [
        model({ linkIdHex: "a", kind: "group", targetId: target, mintedAtMs: 1 }),
        model({ linkIdHex: "b", kind: "group", targetId: target, mintedAtMs: 2, maxUses: 0xffffffff }),
        model({ linkIdHex: "c", kind: "group", targetId: target, revoked: true }),
        model({ linkIdHex: "d", kind: "group", targetId: new Uint8Array(32).fill(6) }),
      ],
      "group",
      "05".repeat(32),
      100,
    );
    expect(out.map((r) => r.linkIdHex)).toEqual(["b", "a"]);
  });
});
