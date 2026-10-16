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
import {
  fakeRegistrarAuthority,
  fakeRootAuthority,
  fakeSigner,
  parseFakeStatement,
} from "./access-test-doubles";
import { NoJournalAuthorityError } from "./ownership";
import { fakeTokenPool } from "../tokens/anon-token-test-doubles";
import { DsClient, DsClientArgError, DsHttpError, journalQuery } from "./client";

const GID = new Uint8Array(32).fill(0xab);
const GID_HEX = "ab".repeat(32);

interface Sent {
  readonly url: string;
  readonly method: string;
  readonly proof: string | undefined;
  readonly wake: string | undefined;
  readonly token: string | undefined;
  readonly authorization: string | undefined;
  readonly credentials: RequestCredentials | undefined;
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
      token: headers["x-lakko-anon-token"],
      authorization: headers["authorization"] ?? headers["Authorization"],
      credentials: init?.credentials,
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
    const { fetchImpl, sent } = fakeFetch([{ status: 201, json: { id: 9, created_at: "t" } }]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.publishGroup(GID, new Uint8Array([1, 2, 3, 4, 5]));

    expect(sent[0]?.proof).toBe(`proof:g@4:POST:/ds/group/${GID_HEX}/messages::5`);
    expect(Array.from(sent[0]?.body ?? [])).toEqual([1, 2, 3, 4, 5]);
  });

  it("a knocks read is signed like a messages read", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    const { fetchImpl, sent } = fakeFetch([EMPTY]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.fetchKnocks(GID, { after: 3, waitSecs: 0 });

    expect(sent[0]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/knocks?after=3&wait=0`);
    expect(sent[0]?.proof).toBe(`proof:g@4:GET:/ds/group/${GID_HEX}/knocks:after=3&wait=0:0`);
  });

  it("surfaces the DS's latest epoch of a claimed journal, and nothing when absent", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    const { fetchImpl } = fakeFetch([
      { status: 200, json: { messages: [], has_more: false, latest_epoch: 5 } },
      EMPTY,
    ]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    const claimed = await ds.fetchGroup(GID, { after: 0 });
    expect(claimed.latestEpoch).toBe(5);
    const unclaimed = await ds.fetchGroup(GID, { after: 0 });
    expect(unclaimed.latestEpoch).toBeUndefined();
    expect("latestEpoch" in unclaimed).toBe(false);
  });

  it("a group nobody on this device holds is read unsigned", async () => {
    const reg = registryWith(() => null);
    const { fetchImpl, sent } = fakeFetch([EMPTY]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).fetchGroup(GID, { after: 0 });
    expect(sent[0]?.proof).toBeUndefined();
  });
});

/** A registry whose factory hands out fake root / registrar authorities and
 *  whose owner source answers `owner`. */
function ownedRegistry(
  signer: () => ReturnType<typeof fakeSigner> | null,
  owner: boolean,
  registrar = false,
): DsAccessRegistry {
  const reg = registryWith(signer);
  reg.registerOwnerSource(() => owner);
  reg.setAuthorityFactory({
    root: () => fakeRootAuthority("root"),
    registrar: () => (registrar ? fakeRegistrarAuthority("reg", 2) : { kind: "none" }),
  });
  return reg;
}

describe("ADR-0057 obligation 2 — the lazy claim is the owner's alone", () => {
  it("no claim on open any more: a request that is not refused sends nothing else", async () => {
    const reg = ownedRegistry(() => fakeSigner(4n), true);
    const { fetchImpl, sent } = fakeFetch([EMPTY]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).fetchGroup(GID, { after: 0 });
    expect(sent.map((s) => s.method)).toEqual(["GET"]);
  });

  it("the owner, refused, claims with its ROOT at the current epoch and retries once", async () => {
    const reg = ownedRegistry(() => fakeSigner(4n), true);
    const { fetchImpl, sent } = fakeFetch([{ status: 401 }, { status: 204 }, EMPTY]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).fetchGroup(GID, { after: 0 });
    expect(sent.map((s) => `${s.method}:${s.url.replace(`/api/ds/ds/group/${GID_HEX}`, "")}`)).toEqual([
      "GET:/messages?after=0",
      "PUT:/claim",
      "GET:/messages?after=0",
    ]);
    // A statement carries its own signature: no proof header.
    expect(sent[1]?.proof).toBeUndefined();
    const st = parseFakeStatement(new TextDecoder().decode(sent[1]?.body));
    expect(st.signer).toBe("root");
    expect(st.epoch).toBe("4");
  });

  it("an owner whose journal is already claimed (409) sends nothing more and surfaces the refusal", async () => {
    const reg = ownedRegistry(() => fakeSigner(4n), true);
    const { fetchImpl, sent } = fakeFetch([{ status: 401 }, { status: 409 }]);
    const err = await new DsClient("/api/ds", { access: reg, fetchImpl })
      .fetchGroup(GID, { after: 0 })
      .catch((e: unknown) => e);
    expect((err as DsHttpError).status).toBe(401);
    expect(sent.map((s) => s.url.split("/").pop())).toEqual(["messages?after=0", "claim"]);
  });

  it("a member (even an admin registrar) never claims and never registers on a refusal", async () => {
    const reg = ownedRegistry(() => fakeSigner(4n), false, true);
    const { fetchImpl, sent } = fakeFetch([{ status: 401 }]);
    const err = await new DsClient("/api/ds", { access: reg, fetchImpl })
      .fetchGroup(GID, { after: 0 })
      .catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DsHttpError);
    expect(sent.map((s) => s.method)).toEqual(["GET"]);
  });
});

describe("ADR-0057 statements", () => {
  it("putStatement PUTs the signer's body to the service path, unsigned", async () => {
    const { fetchImpl, sent } = fakeFetch([{ status: 204 }]);
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl });
    await ds.putStatement(`/ds/group/${GID_HEX}/registrars`, '{"statement_b64":"AA=="}');
    expect(sent[0]?.method).toBe("PUT");
    expect(sent[0]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/registrars`);
    expect(sent[0]?.proof).toBeUndefined();
    expect(new TextDecoder().decode(sent[0]?.body)).toBe('{"statement_b64":"AA=="}');
  });

  it("putStatement refuses a path that does not name a group journal", async () => {
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl: fakeFetch([]).fetchImpl });
    await expect(ds.putStatement("/ds/mailbox/x/access", "{}")).rejects.toBeInstanceOf(DsClientArgError);
  });

  it("registerGroupEpoch signs with the root when this account owns the journal", async () => {
    const reg = ownedRegistry(() => fakeSigner(4n), true);
    // The owner's once-per-session claim goes first (`owner-claim.ts`).
    const { fetchImpl, sent } = fakeFetch([{ status: 204 }, { status: 204 }]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).registerGroupEpoch(
      GID,
      5n,
      new Uint8Array(32).fill(5),
      [new Uint8Array(32).fill(9)],
    );
    expect(sent[0]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/claim`);
    const st = parseFakeStatement(new TextDecoder().decode(sent[1]?.body));
    expect(sent[1]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/access`);
    expect(st.signer).toBe("root");
    expect(st.epoch).toBe("5");
    expect(st.keys).toBe("09".repeat(32));
  });

  it("registerGroupEpoch signs with the registrar seed otherwise", async () => {
    const reg = ownedRegistry(() => fakeSigner(4n), false, true);
    const { fetchImpl, sent } = fakeFetch([{ status: 204 }]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).registerGroupEpoch(GID, 5n, new Uint8Array(32));
    expect(parseFakeStatement(new TextDecoder().decode(sent[0]?.body)).signer).toBe("reg");
  });

  it("registerGroupEpoch with neither refuses before any request", async () => {
    const reg = ownedRegistry(() => fakeSigner(4n), false, false);
    const { fetchImpl, sent } = fakeFetch([]);
    await expect(
      new DsClient("/api/ds", { access: reg, fetchImpl }).registerGroupEpoch(GID, 5n, new Uint8Array(32)),
    ).rejects.toBeInstanceOf(NoJournalAuthorityError);
    expect(sent).toHaveLength(0);
  });

  it("a 409 (the server's lock race) is retried once, then surfaces", async () => {
    const reg = ownedRegistry(() => fakeSigner(4n), true);
    const once = fakeFetch([{ status: 409 }, { status: 204 }]);
    await new DsClient("/api/ds", { access: reg, fetchImpl: once.fetchImpl }).registerGroupEpoch(
      GID,
      5n,
      new Uint8Array(32),
    );
    expect(once.sent).toHaveLength(2);
    const twice = fakeFetch([{ status: 409 }, { status: 409 }]);
    const err = await new DsClient("/api/ds", { access: reg, fetchImpl: twice.fetchImpl })
      .registerGroupEpoch(GID, 5n, new Uint8Array(32))
      .catch((e: unknown) => e);
    expect((err as DsHttpError).status).toBe(409);
    expect(twice.sent).toHaveLength(2);
  });
});

describe("contract point 5 — a refused group request", () => {
  it("is retried once under the PREVIOUS epoch's key", async () => {
    let epoch = 4n;
    const reg = registryWith(() => fakeSigner(epoch));
    reg.groupSigners(GID); // epoch 4 cached
    epoch = 5n; // the device applied a commit; the server does not know 5 yet
    const { fetchImpl, sent } = fakeFetch([{ status: 401 }, EMPTY]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.fetchGroup(GID, { after: 0 });

    expect(sent.map((s) => s.proof?.split(":")[0])).toEqual(["proof", "proof"]);
    expect(sent[0]?.proof).toContain("g@5:");
    expect(sent[1]?.proof).toContain("g@4:");
  });

  it("a final refusal surfaces, and says `check the clock` only when the server's Date is off by more than a minute", async () => {
    const now = Date.parse("2026-09-27T12:00:00Z");
    const reg = registryWith(() => fakeSigner(4n));
    const skewDate = new Date(now + 5 * 60_000).toUTCString();
    const { fetchImpl } = fakeFetch([{ status: 401, date: skewDate }]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl, now: () => now });

    const err = await ds.fetchGroup(GID, { after: 0 }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DsHttpError);
    expect((err as DsHttpError).status).toBe(401);
    expect((err as DsHttpError).clockSkewed).toBe(true);
    expect(dsClockSkewNotice({ cause: err })).toBe(CLOCK_SKEW_NOTICE());

    const inSync = new Date(now + 10_000).toUTCString();
    const second = fakeFetch([{ status: 401, date: inSync }]);
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

  it("a read of this account's own mailbox is signed under its mailbox key", async () => {
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
    expect(new TextDecoder().decode(sent[1]?.body)).toBe('{"mailbox_pk_b64":"m"}');
  });

  it("a rendezvous id is read and written unsigned on its own route (ADR-0060)", async () => {
    const reg = new DsAccessRegistry();
    reg.setMailboxSigner(fakeSigner(0n, "m"));
    const { fetchImpl, sent } = fakeFetch([EMPTY, { status: 201, json: { id: 2, created_at: "t" } }]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });
    await ds.fetchRendezvous(OTHER, { after: 1 });
    await ds.publishRendezvous(OTHER, new Uint8Array([1]));
    const hex = "01".repeat(32);
    expect(sent.map((s) => s.url)).toEqual([
      `/api/ds/ds/rendezvous/${hex}/messages?after=1`,
      `/api/ds/ds/rendezvous/${hex}/messages`,
    ]);
    expect(sent.map((s) => s.proof)).toEqual([undefined, undefined]);
  });

  it("a mailbox write carries the reach proof, and the delegation beside it (ADR-0060)", async () => {
    const reg = new DsAccessRegistry();
    const created = { status: 201, json: { id: 1, created_at: "t" } };
    const { fetchImpl, sent } = fakeFetch([created, created, created]);
    const { pool } = fakeTokenPool();
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl, tokens: pool });
    const hex = "01".repeat(32);
    await ds.publishMailbox(OTHER, new Uint8Array([7]), { signer: fakeSigner(0n, "reach") });
    await ds.publishMailbox(OTHER, new Uint8Array([7]), {
      signer: fakeSigner(0n, "delegate"),
      delegation: "DELEG",
    });
    await ds.publishMailbox(OTHER, new Uint8Array([7]));
    expect(sent[0]?.proof).toBe(`proof:reach@0:POST:/ds/mailbox/${hex}/messages::1`);
    expect(sent[1]?.proof).toBe(`proof:delegate@0:POST:/ds/mailbox/${hex}/messages::1`);
    const headers = (fetchImpl as unknown as { mock: { calls: [unknown, RequestInit][] } }).mock.calls.map(
      (c) => (c[1].headers ?? {}) as Record<string, string>,
    );
    expect(headers[1]?.["x-lakko-ds-delegation"]).toBe("DELEG");
    expect(headers[0]?.["x-lakko-ds-delegation"]).toBeUndefined();
    // No grant: unsigned, which an open (Everyone) mailbox stores.
    expect(sent[2]?.proof).toBeUndefined();
    // ADR-0078: a proof carries no token; the proofless write spends one
    // `mailbox-write` token and goes without credentials.
    expect(sent[0]?.token).toBeUndefined();
    expect(sent[1]?.token).toBeUndefined();
    expect(sent[2]?.token).toBe("tok-1");
    expect(sent[2]?.credentials).toBe("omit");
    expect(sent[2]?.authorization).toBeUndefined();
  });

  it("a proofless mailbox write whose token is refused as spent is retried ONCE with another (ADR-0078)", async () => {
    const { fetchImpl, sent } = fakeFetch([
      { status: 401, json: { error: "anon-token-spent" } },
      { status: 201, json: { id: 9, created_at: "t" } },
    ]);
    const { pool } = fakeTokenPool();
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl, tokens: pool });
    await expect(ds.publishMailbox(OTHER, new Uint8Array([7]))).resolves.toEqual({ id: 9, createdAt: "t" });
    expect(sent.map((s) => s.token)).toEqual(["tok-1", "tok-2"]);
    expect(sent.every((s) => s.proof === undefined)).toBe(true);
  });

  it("refuses a delegation without the delegate's signer, before any request", async () => {
    const { fetchImpl, sent } = fakeFetch([]);
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl });
    await expect(ds.publishMailbox(OTHER, new Uint8Array([1]), { delegation: "D" })).rejects.toBeInstanceOf(
      DsClientArgError,
    );
    expect(sent).toEqual([]);
  });

  it("registers reach keys under the identity proof with the documented body", async () => {
    const reg = new DsAccessRegistry();
    reg.setMailboxSigner(fakeSigner(0n, "m"));
    const { fetchImpl, sent } = fakeFetch([{ status: 204 }]);
    await new DsClient("/api/ds", { access: reg, fetchImpl }).putMailboxReach(OWN, {
      add: [
        { publicKey: new Uint8Array(32).fill(1), expiresAtMs: 99 },
        { publicKey: new Uint8Array(32).fill(2) },
      ],
      remove: [new Uint8Array(32).fill(3)],
      open: false,
    });
    const hex = "6d".repeat(32);
    expect(sent[0]?.method).toBe("PUT");
    expect(sent[0]?.url).toBe(`/api/ds/ds/mailbox/${hex}/reach`);
    expect(sent[0]?.proof?.startsWith(`proof:m@0:PUT:/ds/mailbox/${hex}/reach`)).toBe(true);
    const body = JSON.parse(new TextDecoder().decode(sent[0]?.body)) as {
      add: { reach_pk_b64: string; expires_at_ms?: number }[];
      remove: string[];
      open: boolean;
    };
    expect(body.add[0]?.expires_at_ms).toBe(99);
    expect("expires_at_ms" in (body.add[1] ?? {})).toBe(false);
    expect(body.remove).toHaveLength(1);
    expect(body.open).toBe(false);
  });

  it("the reach check reads 204 as registered and 401 as not, and throws on anything else", async () => {
    const { fetchImpl } = fakeFetch([{ status: 204 }, { status: 401 }, { status: 500 }]);
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl });
    expect(await ds.reachCheck(OTHER, fakeSigner(0n, "r"))).toBe(true);
    expect(await ds.reachCheck(OTHER, fakeSigner(0n, "r"))).toBe(false);
    await expect(ds.reachCheck(OTHER, fakeSigner(0n, "r"))).rejects.toBeInstanceOf(DsHttpError);
  });
});

describe("the knock", () => {
  it("spends one ds-knock token, posts the payloads as standard base64, unsigned and without credentials", async () => {
    const { fetchImpl, sent } = fakeFetch([{ status: 201, json: { ids: [4, 5], epoch: 12 } }]);
    const { pool } = fakeTokenPool();
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl, tokens: pool });

    const res = await ds.knock(GID, [new Uint8Array([0xfa]), new Uint8Array([0xfb, 0xfc])]);

    expect(res).toEqual({ ids: [4, 5], epoch: 12 });
    expect(sent).toHaveLength(1);
    expect(sent[0]?.url).toBe(`/api/ds/ds/group/${GID_HEX}/knock`);
    expect(sent[0]?.proof).toBeUndefined();
    expect(sent[0]?.token).toBe("tok-1");
    expect(sent[0]?.authorization).toBeUndefined();
    expect(sent[0]?.credentials).toBe("omit");
    expect(JSON.parse(new TextDecoder().decode(sent[0]?.body))).toEqual({
      payloads_b64: ["+g==", "+/w="],
    });
    expect(pool.size("ds-knock")).toBeGreaterThan(0);
  });

  it("a spent token is dropped and the knock retried ONCE with another", async () => {
    const { fetchImpl, sent } = fakeFetch([
      { status: 401, json: { error: "anon-token-spent" } },
      { status: 201, json: { ids: [], epoch: 3 } },
    ]);
    const { pool } = fakeTokenPool();
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl, tokens: pool });
    await expect(ds.knock(GID, [])).resolves.toEqual({ ids: [], epoch: 3 });
    expect(sent.map((s) => s.token)).toEqual(["tok-1", "tok-2"]);
  });
});

describe("ADR-0054 — the wake rides the signed group publish", () => {
  /** A stand-in for a sealed wake (ADR-0078): 312 base64url characters. */
  const SEALED = "_".repeat(311) + "8";

  it("adds `x-lakko-wake: <sealed wake>` beside the proof", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    const { fetchImpl, sent } = fakeFetch([{ status: 201, json: { id: 9, created_at: "t" } }]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.publishGroup(GID, new Uint8Array([1, 2]), {
      wake: { headerValue: SEALED },
    });

    expect(sent).toHaveLength(1);
    expect(sent[0]?.wake).toBe(SEALED);
    // The proof is unchanged by the wake: it covers method, path, query, body.
    expect(sent[0]?.proof).toBe(`proof:g@4:POST:/ds/group/${GID_HEX}/messages::2`);
  });

  it("carries the header on every contract-point-5 retry", async () => {
    let epoch = 4n;
    const reg = registryWith(() => fakeSigner(epoch));
    reg.groupSigners(GID);
    epoch = 5n;
    const { fetchImpl, sent } = fakeFetch([
      { status: 401 },
      { status: 201, json: { id: 9, created_at: "t" } },
    ]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.publishGroup(GID, new Uint8Array([1]), { wake: { headerValue: SEALED } });

    expect(sent.map((r) => r.wake)).toEqual([SEALED, SEALED]);
  });

  it("sends no header without a wake, or with `wake: null`", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    const { fetchImpl, sent } = fakeFetch([
      { status: 201, json: { id: 1, created_at: "t" } },
      { status: 201, json: { id: 2, created_at: "t" } },
    ]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await ds.publishGroup(GID, new Uint8Array([1]));
    await ds.publishGroup(GID, new Uint8Array([1]), { wake: null });

    expect(sent.map((r) => r.wake)).toEqual([undefined, undefined]);
  });

  it("refuses a wrong-length sealed wake before any request is made", async () => {
    const reg = registryWith(() => fakeSigner(4n));
    const { fetchImpl, sent } = fakeFetch([]);
    const ds = new DsClient("/api/ds", { access: reg, fetchImpl });

    await expect(
      ds.publishGroup(GID, new Uint8Array([1]), {
        wake: { headerValue: SEALED.slice(1) },
      }),
    ).rejects.toBeInstanceOf(DsClientArgError);
    expect(sent).toHaveLength(0);
  });

  it("never puts a wake on a mailbox publish", async () => {
    const { fetchImpl, sent } = fakeFetch([{ status: 201, json: { id: 1, created_at: "t" } }]);
    const { pool } = fakeTokenPool();
    const ds = new DsClient("/api/ds", { access: new DsAccessRegistry(), fetchImpl, tokens: pool });
    await ds.publishMailbox(GID, new Uint8Array([1]));
    expect(sent[0]?.wake).toBeUndefined();
  });
});
