// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057, the second security pass (finding 4): a new owner keeps signing
 * with its registrar seed until its root is confirmed (`ds/root-held.ts` and
 * the authority choice in `ds/access.ts`).
 */
import { describe, expect, it } from "vitest";

import { DsAccessRegistry } from "./access";
import { fakeRegistrarAuthority, fakeRootAuthority } from "./access-test-doubles";
import { RANK_MEMBER, type JournalAuthority } from "./ownership";
import {
  clearRootHeld,
  clearRootHeldAll,
  isRootHeld,
  markRootHeld,
  type RootHeldStorage,
} from "./root-held";

const GID = new Uint8Array(32).fill(0x33);
const GID_B64 = Buffer.from(GID).toString("base64");

function memStorage(): RootHeldStorage & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => void data.set(k, v),
    removeItem: (k) => void data.delete(k),
  };
}

function labelOf(a: JournalAuthority): string {
  if (a.kind === "none") return "none";
  return parseLabel(a);
}
function parseLabel(a: Exclude<JournalAuthority, { kind: "none" }>): string {
  const req = a.signer.rootTransfer(GID, new Uint8Array(32));
  return (JSON.parse(req.bodyJson) as { signer: string }).signer;
}

describe("the root-held marker", () => {
  it("is per (account, group), survives a reload of the storage, and clears", () => {
    const st = memStorage();
    expect(isRootHeld("a", "g", st)).toBe(false);
    markRootHeld("a", "g", st);
    expect(isRootHeld("a", "g", st)).toBe(true);
    expect(isRootHeld("b", "g", st)).toBe(false);
    expect(isRootHeld("a", "h", st)).toBe(false);
    clearRootHeld("a", "g", st);
    expect(isRootHeld("a", "g", st)).toBe(false);
    markRootHeld("a", "g", st);
    clearRootHeldAll("a", st);
    expect(isRootHeld("a", "g", st)).toBe(false);
  });
});

describe("the new owner signs with its registrar seed until the marker is set", () => {
  function registry(st: RootHeldStorage, withRegistrar = true): DsAccessRegistry {
    const r = new DsAccessRegistry();
    r.setAuthorityFactory({
      root: () => fakeRootAuthority("root"),
      registrar: () => (withRegistrar ? fakeRegistrarAuthority("reg", RANK_MEMBER) : { kind: "none" }),
      rootHeld: () => isRootHeld("acc", GID_B64, st),
      markRootHeld: () => markRootHeld("acc", GID_B64, st),
    });
    r.registerOwnerSource(() => true);
    return r;
  }

  it("owner without the marker and with a registrar seed: the registrar", () => {
    const st = memStorage();
    expect(labelOf(registry(st).journalAuthority(GID))).toBe("reg");
  });

  it("the marker (takeover 204 or probe 204) switches it to the root", () => {
    const st = memStorage();
    const r = registry(st);
    r.markRootHeld(GID);
    expect(labelOf(r.journalAuthority(GID))).toBe("root");
    expect(r.rootHeld(GID)).toBe(true);
  });

  it("an owner with no registrar seed (a group it founded, a DeviceGroup) signs with the root", () => {
    const st = memStorage();
    expect(labelOf(registry(st, false).journalAuthority(GID))).toBe("root");
  });

  it("the outgoing owner's probe authority is its root whatever the role table says", () => {
    const st = memStorage();
    const r = registry(st);
    r.registerOwnerSource(() => false);
    expect(labelOf(r.rootAuthority(GID))).toBe("root");
  });
});
