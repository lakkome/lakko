// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 obligation 2 - the owner claims its journal when it OPENS the
 * group (`owner-claim.ts`), because an unclaimed journal never answers the
 * `401` the lazy claim waits for (the live walk of 2026-09-29: pre-existing
 * groups stayed unclaimed and every sweep's registration was refused).
 *
 * The memo is driven over its ports, then through a real `DsClient` over a
 * scripted fetch, where the order of requests on the wire is the thing under
 * test: the claim precedes the first registration.
 *
 * Discrimination (run): with the `await this.ensureOwnerClaim(groupId)` line
 * removed from `DsClient.registerGroupEpoch`, "a registration in an owned
 * journal is preceded by the owner's claim" fails with the `access` request
 * first on the wire.
 */
import { describe, expect, it, vi } from "vitest";

import { DsAccessRegistry } from "./access";
import { fakeRootAuthority, fakeSigner, parseFakeStatement } from "./access-test-doubles";
import { DsClient, DsHttpError } from "./client";
import { OWNER_CLAIM_RETRY_MS, OwnerClaimMemo, type OwnerClaimPorts } from "./owner-claim";

const GID = new Uint8Array(32).fill(0xcd);
const GID_HEX = "cd".repeat(32);

function ports(over: Partial<OwnerClaimPorts> = {}): OwnerClaimPorts & {
  claims: bigint[];
  claimed: number;
  clock: { now: number };
} {
  const clock = { now: 1_000 };
  const out = {
    claims: [] as bigint[],
    claimed: 0,
    clock,
    ownsJournal: () => true,
    currentEpochKey: () => ({ epoch: 4n, key: new Uint8Array(32).fill(4) }),
    claim: async (_g: Uint8Array, epoch: bigint) => {
      out.claims.push(epoch);
    },
    onClaimed: () => {
      out.claimed += 1;
    },
    now: () => clock.now,
    ...over,
  };
  return out;
}

describe("OwnerClaimMemo - once per session, only for an owner", () => {
  it("an owner claims at the epoch the group is at, once", async () => {
    const memo = new OwnerClaimMemo();
    const p = ports();
    expect(await memo.run(GID, p)).toBe("claimed");
    expect(await memo.run(GID, p)).toBe("settled");
    expect(p.claims).toEqual([4n]);
    expect(p.claimed).toBe(1);
  });

  it("a non-owner never claims, and is not remembered (ownership can arrive later)", async () => {
    const memo = new OwnerClaimMemo();
    let owner = false;
    const p = ports({ ownsJournal: () => owner });
    expect(await memo.run(GID, p)).toBe("not-owner");
    expect(p.claims).toEqual([]);
    owner = true;
    expect(await memo.run(GID, p)).toBe("claimed");
  });

  it("no epoch key, nothing sent", async () => {
    const memo = new OwnerClaimMemo();
    const p = ports({ currentEpochKey: () => null });
    expect(await memo.run(GID, p)).toBe("no-epoch-key");
    expect(p.claims).toEqual([]);
  });

  it("409 (claimed under this root with another chain) settles and is left alone", async () => {
    const memo = new OwnerClaimMemo();
    const p = ports({
      claim: async () => {
        throw new DsHttpError(409, "/claim", "");
      },
    });
    expect(await memo.run(GID, p)).toBe("already-claimed");
    expect(await memo.run(GID, p)).toBe("settled");
    expect(p.claimed).toBe(0);
  });

  it("401 (another root holds it) settles too: the takeover owns that case", async () => {
    const memo = new OwnerClaimMemo();
    const p = ports({
      claim: async () => {
        throw new DsHttpError(401, "/claim", "");
      },
    });
    expect(await memo.run(GID, p)).toBe("claimed-by-another-root");
    expect(await memo.run(GID, p)).toBe("settled");
  });

  it("a network failure is retried on a later open, not sooner than the retry wait", async () => {
    const memo = new OwnerClaimMemo();
    let fail = true;
    const p = ports();
    const claim = p.claim;
    p.claim = async (g, e, k) => {
      if (fail) throw new Error("offline");
      await claim(g, e, k);
    };
    expect(await memo.run(GID, p)).toBe("failed");
    p.clock.now += OWNER_CLAIM_RETRY_MS - 1;
    expect(await memo.run(GID, p)).toBe("deferred");
    fail = false;
    p.clock.now += 1;
    expect(await memo.run(GID, p)).toBe("claimed");
  });

  it("concurrent opens share one request", async () => {
    const memo = new OwnerClaimMemo();
    let release: () => void = () => undefined;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const p = ports();
    const claim = p.claim;
    p.claim = async (g, e, k) => {
      await gate;
      await claim(g, e, k);
    };
    const a = memo.run(GID, p);
    const b = memo.run(GID, p);
    release();
    expect(await a).toBe("claimed");
    expect(await b).toBe("claimed");
    expect(p.claims).toEqual([4n]);
  });
});

/** A fetch double answering from a script and recording `METHOD:segment`. */
function wire(script: number[]): { fetchImpl: typeof fetch; sent: string[]; bodies: string[] } {
  const sent: string[] = [];
  const bodies: string[] = [];
  const fetchImpl = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    sent.push(`${init?.method ?? "GET"}:${String(input).replace(`/api/ds/ds/group/${GID_HEX}/`, "")}`);
    const b = init?.body;
    bodies.push(b === undefined || b === null ? "" : new TextDecoder().decode(b as ArrayBuffer));
    const status = script.shift() ?? 204;
    return new Response(status === 401 ? "proof required" : null, { status });
  }) as unknown as typeof fetch;
  return { fetchImpl, sent, bodies };
}

function ownerRegistry(owner: boolean, held: Set<string>): DsAccessRegistry {
  const reg = new DsAccessRegistry();
  reg.registerGroupSource(() => fakeSigner(4n));
  reg.registerOwnerSource(() => owner);
  reg.setAuthorityFactory({
    root: () => fakeRootAuthority("root"),
    registrar: () => ({ kind: "none" }),
    rootHeld: () => true,
    markRootHeld: (g) => held.add(Array.from(g, (x) => x.toString(16).padStart(2, "0")).join("")),
  });
  return reg;
}

describe("DsClient - the owner claims on open, before any registration", () => {
  it("an owner opening an unclaimed group sends a claim before any sweep registers an epoch", async () => {
    const held = new Set<string>();
    const reg = ownerRegistry(true, held);
    const heard: string[] = [];
    reg.onOwnerClaimed(() => heard.push("claimed"));
    const { fetchImpl, sent, bodies } = wire([204, 204]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    // Opening the group (the ownership provider's reconcile, the DeviceGroup
    // provider, the channel's Manage screen).
    expect(await ds.ensureOwnerClaim(GID)).toBe("claimed");
    // The sweep then registers the staged epoch: no second claim.
    await ds.registerGroupEpoch(GID, 5n, new Uint8Array(32).fill(5));

    expect(sent).toEqual(["PUT:claim", "PUT:access"]);
    const claim = parseFakeStatement(bodies[0] ?? "{}");
    expect(claim.signer).toBe("root");
    expect(claim.epoch).toBe("4");
    // The root is held and the listeners (the registrar reconcile, the sweep
    // backoff) heard it, exactly as after a lazy claim.
    expect(held.has(GID_HEX)).toBe(true);
    expect(heard).toEqual(["claimed"]);
  });

  it("a registration in an owned journal is preceded by the owner's claim", async () => {
    const reg = ownerRegistry(true, new Set());
    const { fetchImpl, sent } = wire([204, 204, 204]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });
    await ds.registerGroupEpoch(GID, 5n, new Uint8Array(32).fill(5));
    await ds.registerGroupEpoch(GID, 6n, new Uint8Array(32).fill(6));
    expect(sent).toEqual(["PUT:claim", "PUT:access", "PUT:access"]);
  });

  it("a 409 claim never turns into a reset, and the registration still goes out", async () => {
    const reg = ownerRegistry(true, new Set());
    const { fetchImpl, sent } = wire([409, 204]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });
    expect(await ds.ensureOwnerClaim(GID)).toBe("already-claimed");
    await ds.registerGroupEpoch(GID, 5n, new Uint8Array(32).fill(5));
    expect(sent).toEqual(["PUT:claim", "PUT:access"]);
  });

  it("a member (not the owner) never claims", async () => {
    const reg = ownerRegistry(false, new Set());
    const { fetchImpl, sent } = wire([]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });
    expect(await ds.ensureOwnerClaim(GID)).toBe("not-owner");
    expect(sent).toEqual([]);
  });

  it("the lazy claim after a refused read settles the session memo", async () => {
    const reg = ownerRegistry(true, new Set());
    // Read refused, lazy claim lands, read retried; then an open sends nothing.
    const { fetchImpl, sent } = wire([401, 204]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });
    await ds.fetchGroup(GID, { after: 0 }).catch(() => undefined);
    expect(await ds.ensureOwnerClaim(GID)).toBe("settled");
    expect(sent.filter((s) => s === "PUT:claim")).toHaveLength(1);
  });
});
