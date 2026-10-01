// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 (amended 2026-10-01) item 1 — the registrar list every member
 * collects from the owner's registrar-seed envelopes, whoever they are
 * addressed to, so an admin that commits a removal can revoke the evictee's
 * registrar key. The envelope is composed by the REAL core.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  clearRegistrarLists,
  loadRegistrarList,
  recordRegistrarListEntry,
  registrarListEntryOf,
  type RegistrarListStorage,
} from "./registrar-list";
import { RANK_ADMIN, RANK_MEMBER, revocationKeys } from "./ownership";
import { fakeRegistrarAuthority } from "./access-test-doubles";
import { wasmRegistrarDerivation } from "./journal-authority";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const GID = new Uint8Array(32).fill(0x51);
const b64 = (b: Uint8Array): string => Buffer.from(b).toString("base64");

function memoryStorage(): RegistrarListStorage & { length: number; key(i: number): string | null } {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    key: (i) => [...map.keys()][i] ?? null,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
  };
}

function envelopeFor(rank: number) {
  const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x52));
  const member = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x53));
  const rootSeed = wasm.dsJournalRootSeed(owner.ed25519Seed, GID);
  const seed = wasm.dsJournalRegistrarSeed(rootSeed, member.signaturePublicKey);
  const plaintext = wasm.composeRegistrarSeedEnvelope(
    GID,
    member.signaturePublicKey,
    member.x25519PublicKey,
    rank,
    seed,
  );
  const d = wasm.decodeRegistrarSeedEnvelope(plaintext);
  const header = {
    journalId: d.journalId,
    memberAccountKey: d.memberAccountKey,
    rank: d.rank,
    registrarPublicKey: d.registrarPublicKey,
  };
  d.free();
  const expectedPk = wasmRegistrarDerivation(wasm).publicKeyOfSeed(seed);
  const out = {
    header,
    ownerKey: owner.signaturePublicKey,
    memberKey: member.signaturePublicKey,
    expectedPk,
  };
  owner.free();
  member.free();
  return out;
}

describe("collecting the list from the owner's envelopes", () => {
  it("any member records addressee -> registrar public key from an envelope the OWNER sent", () => {
    const e = envelopeFor(RANK_MEMBER);
    const got = registrarListEntryOf({
      header: e.header,
      groupId: GID,
      senderAccountKey: e.ownerKey,
      ownerAccountKey: e.ownerKey,
    });
    expect(got?.accountKeyB64).toBe(b64(e.memberKey));
    // The clear header carries exactly the key the owner registered.
    expect(got?.entry).toEqual({ publicKey: e.expectedPk, rank: RANK_MEMBER });
  });

  it("drops an envelope from a non-owner, for another journal, or a root handover", () => {
    const e = envelopeFor(RANK_ADMIN);
    const base = { header: e.header, groupId: GID, senderAccountKey: e.ownerKey, ownerAccountKey: e.ownerKey };
    expect(registrarListEntryOf({ ...base, senderAccountKey: e.memberKey })).toBeNull();
    expect(registrarListEntryOf({ ...base, ownerAccountKey: undefined })).toBeNull();
    expect(registrarListEntryOf({ ...base, groupId: new Uint8Array(32).fill(1) })).toBeNull();
    expect(registrarListEntryOf({ ...base, header: { ...e.header, rank: 3 } })).toBeNull();
    expect(registrarListEntryOf({ ...base, header: null })).toBeNull();
  });

  it("persists per (account, group), a newer entry replaces, and clears per account", () => {
    const st = memoryStorage();
    const k1 = new Uint8Array(32).fill(1);
    const k2 = new Uint8Array(32).fill(2);
    recordRegistrarListEntry("acc", "g", "A", { publicKey: k1, rank: RANK_MEMBER }, st);
    recordRegistrarListEntry("acc", "g", "A", { publicKey: k2, rank: RANK_ADMIN }, st);
    recordRegistrarListEntry("other", "g", "A", { publicKey: k1, rank: RANK_MEMBER }, st);
    expect(loadRegistrarList("acc", "g", st).get("A")).toEqual({ publicKey: k2, rank: RANK_ADMIN });
    expect(loadRegistrarList("acc", "h", st).size).toBe(0);
    clearRegistrarLists("acc", st);
    expect(loadRegistrarList("acc", "g", st).size).toBe(0);
    expect(loadRegistrarList("other", "g", st).size).toBe(1);
  });
});

describe("revocationKeys for a registrar signer", () => {
  const derive = wasmRegistrarDerivation(wasm);
  const memberAcct = new Uint8Array(32).fill(0x61);
  const adminAcct = new Uint8Array(32).fill(0x62);
  const list = new Map([
    [b64(memberAcct), { publicKey: new Uint8Array(32).fill(0x71), rank: RANK_MEMBER }],
    [b64(adminAcct), { publicKey: new Uint8Array(32).fill(0x72), rank: RANK_ADMIN }],
  ]);

  it("an admin revokes a removed member's key and skips an equal-rank admin's", () => {
    const authority = fakeRegistrarAuthority("admin", RANK_ADMIN);
    expect(revocationKeys(authority, [memberAcct, adminAcct], derive, list)).toEqual([
      new Uint8Array(32).fill(0x71),
    ]);
  });

  it("a member-rank signer revokes nothing (nobody ranks below it)", () => {
    const authority = fakeRegistrarAuthority("member", RANK_MEMBER);
    expect(revocationKeys(authority, [memberAcct, adminAcct], derive, list)).toEqual([]);
  });

  it("no authority revokes nothing", () => {
    expect(revocationKeys({ kind: "none" }, [memberAcct], derive, list)).toEqual([]);
  });
});
