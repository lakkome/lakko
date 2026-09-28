// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 — what `DsClient` puts on the wire. The proof covers the SERVICE
 * path (never the `/api/ds` prefix), the raw query byte-for-byte and the exact
 * body, so these tests read the header the fake signer produced and compare it
 * with what the request actually carried. They also pin the contract-point-5
 * retry ladder and which requests stay unsigned.
 */
import { describe, expect, it, vi } from "vitest";

import { DsAccessRegistry, dsClockSkewNotice, CLOCK_SKEW_NOTICE } from "./access";
import { fakeSigner } from "./access-test-doubles";
import { DsClient, DsClientArgError, DsHttpError, journalQuery } from "./client";

const GID = new Uint8Array(32).fill(0xab);
const GID_HEX = "ab".repeat(32);

interface Sent {
  readonly url: string;
  readonly method: string;
  readonly proof: string | undefined;
  readonly wake: string | undefined;
  readonly body: Uint8Array | undefined;
}

function bodyOf(init: RequestInit | undefined): Uint8Array | undefined {
  const b = init?.body;
  if (b === undefined || b === null) return undefined;
  return new Uint8Array(b as ArrayBuffer);
}

/** A fetch double that answers from a script and records every request. */
function fakeFetch(
  script: Array<{ status: number; json?: unknown; date?: string }>,
): { fetchImpl: typeof fetch; sent: Sent[] } {
  const sent: Sent[] = [];
  const fetchImpl = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const headers = (init?.headers ?? {}) as Record<string, string>;
    sent.push({
      url: String(input),
      method: init?.method ?? "GET",
      proof: headers["x-lakko-ds-proof"],
      wake: headers["x-lakko-wake"],
      body: bodyOf(init),
    });
    const next = script.shift() ?? { status: 200, json: { messages: [], has_more: false } };
    const h = new Headers();
    if (next.date !== undefined) h.set("date", next.date);
    return new Response(
      next.json === undefined ? (next.status === 401 ? "proof required" : null) : JSON.stringify(next.json),
      { status: next.status, headers: h },
    );
  }) as unknown as typeof fetch;
  return { fetchImpl, sent };
}

function registryWith(signer: () => ReturnType<typeof fakeSigner> | null): DsAccessRegistry {
  const reg = new DsAccessRegistry();
  reg.registerGroupSource(() => signer());
  return reg;
}

const EMPTY = { status: 200, json: { messages: [], has_more: false } };

describe("contract point 1 — a group read is signed over the service path and the exact query", () => {
  it("signs `/ds/group/<hex>/messages` and the query the URL carries, over an empty body", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    reg.markClaimAttempt(GID); // keep the claim-on-open PUT out of this test
    const { fetchImpl, sent } = fakeFetch([EMPTY]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.fetchGroup(GID, { after: 17, waitSecs: 25 });

    expect(sent).toHaveLength(1);
    const query = journalQuery({ after: 17, waitSecs: 25 });
    expect(query).toBe("after=17&wait=25");
    expect(sent[0]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/messages?${query}`);
    // The header names the SERVICE path (no `/api/ds`) and the identical query.
    expect(sent[0]?.proof).toBe(`proof:g@4:GET:/ds/group/${GID_HEX}/messages:${query}:0`);
  });

  it("a publish signs the exact body bytes it sends", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    reg.markClaimAttempt(GID);
    const { fetchImpl, sent } = fakeFetch([{ status: 201, json: { id: 9, created_at: "t" } }]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.publishGroup(GID, new Uint8Array([1, 2, 3, 4, 5]));

    expect(sent[0]?.proof).toBe(`proof:g@4:POST:/ds/group/${GID_HEX}/messages::5`);
    expect(Array.from(sent[0]?.body ?? [])).toEqual([1, 2, 3, 4, 5]);
  });

  it("a knocks read is signed like a messages read", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    reg.markClaimAttempt(GID);
    const { fetchImpl, sent } = fakeFetch([EMPTY]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.fetchKnocks(GID, { after: 3, waitSecs: 0 });

    expect(sent[0]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/knocks?after=3&wait=0`);
    expect(sent[0]?.proof).toBe(`proof:g@4:GET:/ds/group/${GID_HEX}/knocks:after=3&wait=0:0`);
  });

  it("a group nobody on this device holds is read unsigned", async () => {
    const reg = registryWith(() => null);
    const { fetchImpl, sent } = fakeFetch([EMPTY]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).fetchGroup(GID, { after: 0 });
    expect(sent[0]?.proof).toBeUndefined();
  });
});

describe("contract point 2 — claim on open", () => {
  it("the first request for a group in a session also claims it, unsigned, with the signer's claim body", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    // The claim is fired before the read's own attempt, so it takes the
    // first scripted answer.
    const { fetchImpl, sent } = fakeFetch([{ status: 204 }, EMPTY, EMPTY]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.fetchGroup(GID, { after: 0 });
    await new Promise((r) => setTimeout(r, 0));
    await ds.fetchGroup(GID, { after: 0 });
    await new Promise((r) => setTimeout(r, 0));

    const claims = sent.filter((s) => s.method === "PUT");
    expect(claims).toHaveLength(1);
    expect(claims[0]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/access`);
    expect(claims[0]?.proof).toBeUndefined();
    expect(new TextDecoder().decode(claims[0]?.body)).toBe('{"epoch":4,"access_pk_b64":"g4"}');
  });
});

describe("contract point 5 — a refused group request", () => {
  it("is retried once under the PREVIOUS epoch's key", async () => {
    let epoch = 4n;
    const reg = registryWith(() => fakeSigner(epoch));
    reg.markClaimAttempt(GID);
    reg.groupSigners(GID); // epoch 4 cached
    epoch = 5n; // the device applied a commit; the server does not know 5 yet
    const { fetchImpl, sent } = fakeFetch([{ status: 401 }, EMPTY]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.fetchGroup(GID, { after: 0 });

    expect(sent.map((s) => s.proof?.split(":")[0])).toEqual(["proof", "proof"]);
    expect(sent[0]?.proof).toContain("g@5:");
    expect(sent[1]?.proof).toContain("g@4:");
  });

  it("then once more after a claim signed by the previous key", async () => {
    let epoch = 4n;
    const reg = registryWith(() => fakeSigner(epoch));
    reg.markClaimAttempt(GID);
    reg.groupSigners(GID);
    epoch = 5n;
    const { fetchImpl, sent } = fakeFetch([{ status: 401 }, { status: 401 }, { status: 204 }, EMPTY]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.fetchGroup(GID, { after: 0 });

    expect(sent.map((s) => `${s.method}:${s.proof?.split(":")[1] ?? "-"}`)).toEqual([
      "GET:g@5",
      "GET:g@4",
      "PUT:g@4", // the claim of epoch 5, proven under epoch 4
      "GET:g@5",
    ]);
    expect(new TextDecoder().decode(sent[2]?.body)).toBe('{"epoch":5,"access_pk_b64":"g5"}');
  });

  it("a final refusal surfaces, and says `check the clock` only when the server's Date is off by more than a minute", async () => {
    const now = Date.parse("2026-09-27T12:00:00Z");
    const reg = registryWith(() => fakeSigner(4n));
    reg.markClaimAttempt(GID);
    const skewDate = new Date(now + 5 * 60_000).toUTCString();
    const { fetchImpl } = fakeFetch([
      { status: 401, date: skewDate },
      { status: 204 },
      { status: 401, date: skewDate },
    ]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl, now: () => now });

    const err = await ds.fetchGroup(GID, { after: 0 }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DsHttpError);
    expect((err as DsHttpError).status).toBe(401);
    expect((err as DsHttpError).clockSkewed).toBe(true);
    expect(dsClockSkewNotice({ cause: err })).toBe(CLOCK_SKEW_NOTICE);

    const inSync = new Date(now + 10_000).toUTCString();
    const second = fakeFetch([
      { status: 401, date: inSync },
      { status: 204 },
      { status: 401, date: inSync },
    ]);
    const ds2 = new DsClient("/api/ds", { access: reg, fetchImpl: second.fetchImpl, now: () => now });
    const err2 = await ds2.fetchGroup(GID, { after: 0 }).catch((e: unknown) => e);
    expect((err2 as DsHttpError).clockSkewed).toBe(false);
    expect(dsClockSkewNotice(err2)).toBeNull();
  });

  it("an explicit signer gets exactly one attempt", async () => {
    const reg = registryWith(() => fakeSigner(9n));
    const { fetchImpl, sent } = fakeFetch([{ status: 401 }]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });
    await expect(
      ds.publishGroup(GID, new Uint8Array([1]), { signer: fakeSigner(3n, "x") }),
    ).rejects.toBeInstanceOf(DsHttpError);
    expect(sent).toHaveLength(1);
    expect(sent[0]?.proof).toContain("x@3:POST");
  });
});

describe("mailboxes", () => {
  const OWN = new Uint8Array(32).fill("m".charCodeAt(0));
  const OTHER = new Uint8Array(32).fill(0x01);

  it("a read of this device's own mailbox is signed under its identity key", async () => {
    const reg = new DsAccessRegistry();
    reg.setMailboxSigner(fakeSigner(0n, "m"));
    const { fetchImpl, sent } = fakeFetch([EMPTY]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).fetchMailbox(OWN, { after: 2 });
    const hex = "6d".repeat(32);
    expect(sent[0]?.url).toBe(`/api/ds/ds/mailbox/${hex}/messages?after=2`);
    expect(sent[0]?.proof).toBe(`proof:m@0:GET:/ds/mailbox/${hex}/messages:after=2:0`);
  });

  it("a refused own-mailbox read claims the mailbox and retries once", async () => {
    const reg = new DsAccessRegistry();
    reg.setMailboxSigner(fakeSigner(0n, "m"));
    const { fetchImpl, sent } = fakeFetch([{ status: 401 }, { status: 204 }, EMPTY]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).fetchMailbox(OWN, {});
    expect(sent.map((s) => s.method)).toEqual(["GET", "PUT", "GET"]);
    expect(sent[1]?.proof).toBeUndefined();
    expect(new TextDecoder().decode(sent[1]?.body)).toBe('{"identity_pk_b64":"m"}');
  });

  it("someone else's mailbox and a rendezvous mailbox are read and written unsigned", async () => {
    const reg = new DsAccessRegistry();
    reg.setMailboxSigner(fakeSigner(0n, "m"));
    const { fetchImpl, sent } = fakeFetch([EMPTY, { status: 201, json: { id: 1, created_at: "t" } }, { status: 201, json: { id: 2, created_at: "t" } }]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });
    await ds.fetchMailbox(OTHER, {});
    await ds.publishMailbox(OTHER, new Uint8Array([1]));
    // Even the device's OWN mailbox is written unsigned: a mailbox write is
    // the open cold inbox.
    await ds.publishMailbox(OWN, new Uint8Array([1]));
    expect(sent.map((s) => s.proof)).toEqual([undefined, undefined, undefined]);
  });
});

describe("the knock", () => {
  it("posts the proof of work and the payloads as standard base64, unsigned", async () => {
    const { fetchImpl, sent } = fakeFetch([
      { status: 200, json: { seed: "c2VlZA==", effort_bits: 8, expires_in: 60 } },
      { status: 201, json: { ids: [4, 5], epoch: 12 } },
    ]);
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl });

    const ch = await ds.powChallenge();
    expect(ch).toEqual({ seed: "c2VlZA==", effortBits: 8 });
    const res = await ds.knock(GID, { seed: "c2VlZA==", nonce: "bg==", solution: "cw==" }, [
      new Uint8Array([0xfa]),
      new Uint8Array([0xfb, 0xfc]),
    ]);

    expect(res).toEqual({ ids: [4, 5], epoch: 12 });
    expect(sent[0]?.url).toBe("/api/ds/pow/challenge");
    expect(sent[1]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/knock`);
    expect(sent[1]?.proof).toBeUndefined();
    expect(JSON.parse(new TextDecoder().decode(sent[1]?.body))).toEqual({
      pow: { seed: "c2VlZA==", nonce: "bg==", solution: "cw==" },
      payloads_b64: ["+g==", "+/w="],
    });
  });
});

describe("ADR-0054 — the wake rides the signed group publish", () => {
  const PUSH_GID = new Uint8Array(32).fill(0xff);
  const PUSH_GID_B64URL = "_".repeat(42) + "8";

  it("adds `x-lakko-wake: <push_gid>;<kind>` beside the proof", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    reg.markClaimAttempt(GID);
    const { fetchImpl, sent } = fakeFetch([{ status: 201, json: { id: 9, created_at: "t" } }]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.publishGroup(GID, new Uint8Array([1, 2]), {
      wake: { pushGid: PUSH_GID, kind: "message" },
    });

    expect(sent).toHaveLength(1);
    expect(sent[0]?.wake).toBe(`${PUSH_GID_B64URL};message`);
    // The proof is unchanged by the wake: it covers method, path, query, body.
    expect(sent[0]?.proof).toBe(`proof:g@4:POST:/ds/group/${GID_HEX}/messages::2`);
  });

  it("carries the header on every contract-point-5 retry", async () => {
    let epoch = 4n;
    const reg = registryWith(() => fakeSigner(epoch));
    reg.markClaimAttempt(GID);
    reg.groupSigners(GID);
    epoch = 5n;
    const { fetchImpl, sent } = fakeFetch([
      { status: 401 },
      { status: 201, json: { id: 9, created_at: "t" } },
    ]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.publishGroup(GID, new Uint8Array([1]), { wake: { pushGid: PUSH_GID, kind: "call" } });

    expect(sent.map((r) => r.wake)).toEqual([
      `${PUSH_GID_B64URL};call`,
      `${PUSH_GID_B64URL};call`,
    ]);
  });

  it("sends no header without a wake, or with `wake: null`", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    reg.markClaimAttempt(GID);
    const { fetchImpl, sent } = fakeFetch([
      { status: 201, json: { id: 1, created_at: "t" } },
      { status: 201, json: { id: 2, created_at: "t" } },
    ]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.publishGroup(GID, new Uint8Array([1]));
    await ds.publishGroup(GID, new Uint8Array([1]), { wake: null });

    expect(sent.map((r) => r.wake)).toEqual([undefined, undefined]);
  });

  it("refuses a wrong-length push_gid before any request is made", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    reg.markClaimAttempt(GID);
    const { fetchImpl, sent } = fakeFetch([]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await expect(
      ds.publishGroup(GID, new Uint8Array([1]), {
        wake: { pushGid: new Uint8Array(31), kind: "message" },
      }),
    ).rejects.toBeInstanceOf(DsClientArgError);
    expect(sent).toHaveLength(0);
  });

  it("never puts a wake on a mailbox publish", async () => {
    const { fetchImpl, sent } = fakeFetch([{ status: 201, json: { id: 1, created_at: "t" } }]);
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl });
    await ds.publishMailbox(GID, new Uint8Array([1]));
    expect(sent[0]?.wake).toBeUndefined();
  });
});
