// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 section 4: a revoked link's ledger row is deleted once, by the
 * device that revokes it. The panel and the register sweep both reach
 * `deleteRevokedLinkRow`; the second call while the first runs joins it,
 * and a call once the row is deleted sends nothing. A link another device
 * revoked only loses this device's local record.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const minted = vi.hoisted(() => ({
  rows: [] as unknown[],
  forgotten: [] as string[],
  reachRevoked: 0,
}));

vi.mock("./minted-links", () => ({
  listMintedLinks: async () => minted.rows,
  forgetMintedLink: async (_account: string, _kind: string, slug: string) => {
    minted.forgotten.push(slug);
  },
}));

vi.mock("../reach/reach-service", () => ({
  reachSession: () => ({
    revokeLink: async () => {
      minted.reachRevoked += 1;
    },
  }),
}));

import {
  deleteRevokedLinkRow,
  forgetLinksRevokedElsewhere,
  owedRowDeletions,
  setOwedRowDeletion,
} from "./own-links";
import type { DiscoveryClient } from "../discovery/client";

class MemoryStorage {
  private readonly map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
}

beforeEach(() => {
  (globalThis as unknown as { localStorage: unknown }).localStorage = new MemoryStorage();
  minted.rows = [];
  minted.forgotten = [];
  minted.reachRevoked = 0;
});

afterEach(() => {
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

const ACCOUNT = "acct";
const LINK_HEX = "ab".repeat(16);
const RECORD = {
  linkIdHex: LINK_HEX,
  kind: "contact" as const,
  ledgerId: new Uint8Array(6).fill(1),
  revocationSeed: new Uint8Array(32).fill(2),
};

/** A ledger that counts the deletions it is asked for. */
function ledger(answer: () => Promise<void> = async () => undefined) {
  const calls = { contact: 0, call: 0 };
  const discovery = {
    revokeInvite: async () => {
      calls.contact += 1;
      await answer();
      return { kind: "revoked" as const };
    },
    revokeCallInvite: async () => {
      calls.call += 1;
      await answer();
      return { kind: "revoked" as const };
    },
  };
  return { calls, discovery: discovery as unknown as DiscoveryClient };
}

function networkError(): Error {
  const err = new Error("offline");
  err.name = "DiscoveryNetworkError";
  return err;
}

describe("deleteRevokedLinkRow", () => {
  it("the panel and the sweep together send one deletion", async () => {
    const { calls, discovery } = ledger();
    setOwedRowDeletion(ACCOUNT, LINK_HEX, true);
    const [a, b] = await Promise.all([
      deleteRevokedLinkRow(discovery, ACCOUNT, RECORD),
      deleteRevokedLinkRow(discovery, ACCOUNT, RECORD),
    ]);
    expect([a, b]).toEqual(["done", "done"]);
    expect(calls.contact).toBe(1);
    expect(owedRowDeletions(ACCOUNT).has(LINK_HEX)).toBe(false);
  });

  it("once the row is deleted, a later call sends nothing", async () => {
    const { calls, discovery } = ledger();
    setOwedRowDeletion(ACCOUNT, LINK_HEX, true);
    await deleteRevokedLinkRow(discovery, ACCOUNT, RECORD);
    expect(await deleteRevokedLinkRow(discovery, ACCOUNT, RECORD)).toBe("done");
    expect(calls.contact).toBe(1);
  });

  it("a deletion that is not owed sends nothing", async () => {
    const { calls, discovery } = ledger();
    expect(await deleteRevokedLinkRow(discovery, ACCOUNT, RECORD)).toBe("done");
    expect(calls.contact).toBe(0);
  });

  it("an unreachable ledger keeps the deletion owed, and the next call retries it", async () => {
    let offline = true;
    const { calls, discovery } = ledger(async () => {
      if (offline) throw networkError();
    });
    setOwedRowDeletion(ACCOUNT, LINK_HEX, true);
    expect(await deleteRevokedLinkRow(discovery, ACCOUNT, RECORD)).toBe("unreachable");
    expect(owedRowDeletions(ACCOUNT).has(LINK_HEX)).toBe(true);
    offline = false;
    expect(await deleteRevokedLinkRow(discovery, ACCOUNT, RECORD)).toBe("done");
    expect(calls.contact).toBe(2);
    expect(owedRowDeletions(ACCOUNT).has(LINK_HEX)).toBe(false);
  });

  it("a call link goes to the call ledger", async () => {
    const { calls, discovery } = ledger();
    setOwedRowDeletion(ACCOUNT, LINK_HEX, true);
    await deleteRevokedLinkRow(discovery, ACCOUNT, { ...RECORD, kind: "call" });
    expect(calls).toEqual({ contact: 0, call: 1 });
  });
});

describe("forgetLinksRevokedElsewhere", () => {
  const localRow = {
    kind: "contact",
    blobIdSlug: "AQEBAQEB",
    linkId: new Uint8Array(16).fill(0xab),
    revocationSeed: new Uint8Array(32).fill(3),
    mintedAtMs: 1,
  };

  it("a link revoked on another device: the local record goes, the ledger is not asked", async () => {
    const { calls, discovery } = ledger();
    minted.rows = [localRow];
    expect(
      await forgetLinksRevokedElsewhere(discovery, ACCOUNT, [
        { linkIdHex: LINK_HEX, revocationSeed: RECORD.revocationSeed },
      ]),
    ).toBe(0);
    expect(calls.contact).toBe(0);
    expect(minted.forgotten).toEqual([localRow.blobIdSlug]);
    expect(minted.reachRevoked).toBe(1);
  });

  it("a link not revoked keeps its record", async () => {
    const { calls, discovery } = ledger();
    minted.rows = [localRow];
    await forgetLinksRevokedElsewhere(discovery, ACCOUNT, [
      { linkIdHex: "cd".repeat(16), revocationSeed: RECORD.revocationSeed },
    ]);
    expect(calls.contact).toBe(0);
    expect(minted.forgotten).toEqual([]);
  });
});
