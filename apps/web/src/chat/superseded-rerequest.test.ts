// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0050 section 2.5a point 4 — re-requesting an own membership request a
 * merged commit dropped.
 *
 * Three layers, each pinned here:
 *
 * 1. the verdict (`supersededVerdict`): per kind, what counts as landed, as
 *    still pending, as superseded — keyed on signature / account keys, never
 *    on leaf indices;
 * 2. the driver (`runSupersededRerequests`): fires only on an observed merged
 *    commit, exactly once per commit, with no attempt cap, and stops by state;
 * 3. the re-issue glue (`reissueOwnRequest`): re-derives the request against
 *    the CURRENT group — a removal resolves the subject's current leaf, a role
 *    change hands over only its delta.
 *
 * Discrimination, run and reverted: gating knocks on the committer rank again
 * fails the plain-member knock test in `ds-access-poll.test.ts`; dropping the
 * epoch gate fails four tests here (incl. "no commit: nothing fires"); dropping
 * the synchronous claim and the in-flight guard fails "exactly one re-request,
 * even with two passes racing" and the RetryLater test.
 *
 * Refusals are built from the REAL core's codes and dispositions (the wasm
 * module is initialised below), so "a typed refusal with disposition
 * NotPermitted / DoNotRetry stops it" is asserted against the core's own
 * classification, not a list in this file.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import init, {
  membershipRefusalDisposition,
  membershipRefusalRetryMaySucceed,
} from "../../wasm/lakko_ffi_wasm";
import {
  type IntentSnapshot,
  type OwnRequest,
  type OwnRequestRecord,
  loadOwnRequests,
  ownRequestKey,
  recordOwnRequest,
  runSupersededRerequests,
  supersededVerdict,
} from "./membership-retry";
import { loadUnapplied } from "./unapplied-requests";
import { reissueOwnRequest, type ReissueHost } from "./rerequest-membership";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

class MemoryStorage {
  private readonly map = new Map<string, string>();
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return [...this.map.keys()][i] ?? null;
  }
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
});
afterEach(() => {
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
});

/** A refusal exactly as `lakko-ffi-wasm` throws it, wrapped the way
 *  `ContactsError.wrap` wraps it on the production path. */
function refusal(reason: string): Error {
  const inner = new Error("core prose");
  Object.defineProperty(inner, "refusalReason", { value: reason, enumerable: true });
  Object.defineProperty(inner, "refusalDisposition", {
    value: membershipRefusalDisposition(reason),
    enumerable: true,
  });
  const outer = new Error("contacts: request failed");
  Object.defineProperty(outer, "variant", { value: { cause: inner }, enumerable: true });
  return outer;
}

const ACCT = "acct-1";
const GROUP = "group-1";

// People are named by ACCOUNT key; their devices by leaf key. All 32 bytes,
// like the real keys (the roster reader ignores a malformed account key).
const ALICE = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="; // us, the owner
const ALICE_LEAF = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";
const BOB = "AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM=";
const BOB_LEAF = "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ=";
const CAROL = "BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU=";
const CAROL_LEAF = "BgYGBgYGBgYGBgYGBgYGBgYGBgYGBgYGBgYGBgYGBgY=";
const DAVE = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";
const DAVE_DEVICE = "CAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAg=";
const OTHER_LEAF = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";

/** Alice (leaf 0) and Bob (leaf 1); nothing pending; Alice owns the group. */
function snap(epoch: bigint, over: Partial<IntentSnapshot> = {}): IntentSnapshot {
  return {
    roster: [
      { leafIndex: 0, signatureKeyB64: ALICE_LEAF, accountKeyB64: ALICE },
      { leafIndex: 1, signatureKeyB64: BOB_LEAF, accountKeyB64: BOB },
    ],
    pendingKeys: [],
    pendingCount: 0,
    roleTable: { ownerKeyB64: ALICE, adminKeysB64: [] },
    membershipEnded: false,
    retainedJoinKeys: [],
    epoch,
    ...over,
  };
}

function rec(request: OwnRequest, epoch: bigint, deviceKeysB64?: string[]): OwnRequestRecord {
  return {
    request,
    epoch: epoch.toString(),
    ...(deviceKeysB64 === undefined ? {} : { deviceKeysB64 }),
  };
}

const ADD_DAVE: OwnRequest = { kind: "add", subjectKeyB64: DAVE, subjectAccountIdB64: "ZGF2ZS1pZA==" };
const REMOVE_BOB: OwnRequest = { kind: "remove", subjectKeyB64: BOB };
const LEAVE: OwnRequest = { kind: "leave", subjectKeyB64: ALICE };
const PROMOTE_BOB: OwnRequest = { kind: "set-admin", subjectKeyB64: BOB, makeAdmin: true };
const DEMOTE_BOB: OwnRequest = { kind: "set-admin", subjectKeyB64: BOB, makeAdmin: false };
const TRANSFER_TO_BOB: OwnRequest = {
  kind: "transfer-ownership",
  subjectKeyB64: BOB,
  outgoing: "admin",
};
const APPROVE_CAROL: OwnRequest = { kind: "approve-join", subjectKeyB64: CAROL_LEAF };

// ---------------------------------------------------------------------------
// 1. The verdict
// ---------------------------------------------------------------------------

describe("nothing is decided without a merged commit", () => {
  it("a record at the current epoch is left alone, whatever the group looks like", () => {
    for (const r of [ADD_DAVE, REMOVE_BOB, PROMOTE_BOB, TRANSFER_TO_BOB, APPROVE_CAROL]) {
      expect(supersededVerdict(rec(r, 5n), snap(5n))).toEqual({ kind: "no-commit" });
    }
  });

  it("a torn roster read claims nothing", () => {
    expect(supersededVerdict(rec(ADD_DAVE, 5n), snap(6n, { roster: null }))).toEqual({
      kind: "no-commit",
    });
  });
});

describe("superseded detection — add", () => {
  it("a commit that did not carry the add: re-request", () => {
    expect(supersededVerdict(rec(ADD_DAVE, 5n, [DAVE_DEVICE]), snap(6n))).toEqual({
      kind: "rerequest",
    });
  });

  it("landed when the person's ACCOUNT key is in the roster (the roster lists leaves)", () => {
    const roster = [
      ...(snap(6n).roster ?? []),
      { leafIndex: 2, signatureKeyB64: OTHER_LEAF, accountKeyB64: DAVE },
    ];
    expect(supersededVerdict(rec(ADD_DAVE, 5n), snap(6n, { roster }))).toEqual({
      kind: "landed",
    });
  });

  it("landed when one of the proposed device leaves is in the roster", () => {
    const roster = [
      ...(snap(6n).roster ?? []),
      { leafIndex: 2, signatureKeyB64: DAVE_DEVICE },
    ];
    expect(supersededVerdict(rec(ADD_DAVE, 5n, [DAVE_DEVICE]), snap(6n, { roster }))).toEqual({
      kind: "landed",
    });
  });

  it("kept when the add of one of the person's devices is pending again", () => {
    expect(
      supersededVerdict(
        rec(ADD_DAVE, 5n, [DAVE_DEVICE]),
        snap(6n, { pendingKeys: [`add:${DAVE_DEVICE}`], pendingCount: 1 }),
      ),
    ).toEqual({ kind: "still-pending" });
  });
});

describe("superseded detection — remove, by key and never by leaf", () => {
  it("Bob still in the group after a commit: re-request", () => {
    expect(supersededVerdict(rec(REMOVE_BOB, 5n), snap(6n))).toEqual({ kind: "rerequest" });
  });

  it("Bob gone: landed", () => {
    const roster = [{ leafIndex: 0, signatureKeyB64: ALICE_LEAF, accountKeyB64: ALICE }];
    expect(supersededVerdict(rec(REMOVE_BOB, 5n), snap(6n, { roster }))).toEqual({
      kind: "landed",
    });
  });

  /**
   * The reused-leaf case. Bob's old leaf 1 now belongs to Carol and Bob sits at
   * leaf 2; a pending `remove:1` is a removal of CAROL. A leaf-keyed record
   * would read Bob's removal as still pending (or as landed); the key-keyed one
   * sees Bob present and nothing pending against him.
   */
  it("a vacated-and-reused leaf does not make Carol's pending removal look like ours", () => {
    const roster = [
      { leafIndex: 0, signatureKeyB64: ALICE_LEAF, accountKeyB64: ALICE },
      { leafIndex: 1, signatureKeyB64: CAROL_LEAF, accountKeyB64: CAROL },
      { leafIndex: 2, signatureKeyB64: BOB_LEAF, accountKeyB64: BOB },
    ];
    expect(
      supersededVerdict(
        rec(REMOVE_BOB, 5n),
        snap(6n, { roster, pendingKeys: ["remove:1"], pendingCount: 1 }),
      ),
    ).toEqual({ kind: "rerequest" });
    // And with Bob's CURRENT leaf pending, it is ours.
    expect(
      supersededVerdict(
        rec(REMOVE_BOB, 5n),
        snap(6n, { roster, pendingKeys: ["remove:2"], pendingCount: 1 }),
      ),
    ).toEqual({ kind: "still-pending" });
  });
});

describe("superseded detection — role change as a delta", () => {
  it("promote Bob, table unchanged after a commit: re-request", () => {
    expect(supersededVerdict(rec(PROMOTE_BOB, 5n), snap(6n))).toEqual({ kind: "rerequest" });
  });

  /** The delta is the test, not a table: someone else's change landing
   *  meanwhile (Carol promoted) does not hide that Bob's promotion did too. */
  it("landed once Bob is an admin, whatever else the table now says", () => {
    const roster = [
      ...(snap(6n).roster ?? []),
      { leafIndex: 2, signatureKeyB64: CAROL_LEAF, accountKeyB64: CAROL },
    ];
    expect(
      supersededVerdict(
        rec(PROMOTE_BOB, 5n),
        snap(6n, { roster, roleTable: { ownerKeyB64: ALICE, adminKeysB64: [CAROL, BOB] } }),
      ),
    ).toEqual({ kind: "landed" });
    expect(
      supersededVerdict(
        rec(DEMOTE_BOB, 5n),
        snap(6n, { roster, roleTable: { ownerKeyB64: ALICE, adminKeysB64: [CAROL] } }),
      ),
    ).toEqual({ kind: "landed" });
  });

  it("transfer landed once Bob owns the group", () => {
    expect(
      supersededVerdict(
        rec(TRANSFER_TO_BOB, 5n),
        snap(6n, { roleTable: { ownerKeyB64: BOB, adminKeysB64: [ALICE] } }),
      ),
    ).toEqual({ kind: "landed" });
  });

  it("the subject left: cleared, not re-requested", () => {
    const roster = [{ leafIndex: 0, signatureKeyB64: ALICE_LEAF, accountKeyB64: ALICE }];
    expect(supersededVerdict(rec(PROMOTE_BOB, 5n), snap(6n, { roster }))).toEqual({
      kind: "subject-left",
    });
    expect(supersededVerdict(rec(TRANSFER_TO_BOB, 5n), snap(6n, { roster }))).toEqual({
      kind: "subject-left",
    });
  });

  it("kept while a role change is pending (RFC 9420: one per commit)", () => {
    expect(
      supersededVerdict(
        rec(PROMOTE_BOB, 5n),
        snap(6n, { pendingKeys: ["role-change"], pendingCount: 1 }),
      ),
    ).toEqual({ kind: "still-pending" });
  });
});

describe("superseded detection — leave", () => {
  it("still in the group after a commit: re-request", () => {
    expect(supersededVerdict(rec(LEAVE, 5n), snap(6n))).toEqual({ kind: "rerequest" });
  });

  it("our membership ended: the leave landed", () => {
    expect(supersededVerdict(rec(LEAVE, 5n), snap(6n, { membershipEnded: true }))).toEqual({
      kind: "landed",
    });
  });

  it("our membership ended under any other request: it stops", () => {
    expect(
      supersededVerdict(rec(REMOVE_BOB, 5n), snap(6n, { membershipEnded: true })),
    ).toEqual({ kind: "stop", why: "membership-ended" });
  });
});

describe("superseded detection — approved join", () => {
  it("the joiner asked again (retained): re-approve", () => {
    expect(
      supersededVerdict(rec(APPROVE_CAROL, 5n), snap(6n, { retainedJoinKeys: [CAROL_LEAF] })),
    ).toEqual({ kind: "rerequest" });
  });

  it("nothing retained to approve from: it stops", () => {
    expect(supersededVerdict(rec(APPROVE_CAROL, 5n), snap(6n))).toEqual({
      kind: "stop",
      why: "join-request-gone",
    });
  });

  it("the joiner is in: landed", () => {
    const roster = [
      ...(snap(6n).roster ?? []),
      { leafIndex: 2, signatureKeyB64: CAROL_LEAF, accountKeyB64: CAROL },
    ];
    expect(supersededVerdict(rec(APPROVE_CAROL, 5n), snap(6n, { roster }))).toEqual({
      kind: "landed",
    });
  });
});

// ---------------------------------------------------------------------------
// 2. The driver
// ---------------------------------------------------------------------------

interface Harness {
  epoch: bigint;
  over: Partial<IntentSnapshot>;
  calls: OwnRequest[];
  fail: Error | null;
}

function harness(): Harness {
  return { epoch: 5n, over: {}, calls: [], fail: null };
}

function pass(h: Harness): ReturnType<typeof runSupersededRerequests> {
  return runSupersededRerequests({
    accountIdB64: ACCT,
    groupIdB64: GROUP,
    snapshot: () => snap(h.epoch, h.over),
    rerequest: async (request) => {
      h.calls.push(request);
      // The real propose path records again only after a network round trip;
      // a double that recorded synchronously would hide a missing claim.
      await new Promise((resolve) => setTimeout(resolve, 0));
      if (h.fail !== null) throw h.fail;
      // What the propose path does on success: record again at this epoch.
      recordOwnRequest(ACCT, GROUP, request, h.epoch);
    },
    retryMaySucceed: membershipRefusalRetryMaySucceed,
  });
}

describe("the driver fires only on an observed merged commit", () => {
  it("no commit: nothing fires, however many passes run", async () => {
    const h = harness();
    recordOwnRequest(ACCT, GROUP, REMOVE_BOB, 5n);
    for (let i = 0; i < 10; i++) await pass(h);
    expect(h.calls).toEqual([]);
    expect(loadOwnRequests(ACCT, GROUP).size).toBe(1);
  });

  it("one commit: exactly one re-request, even with two passes racing", async () => {
    const h = harness();
    recordOwnRequest(ACCT, GROUP, REMOVE_BOB, 5n);
    h.epoch = 6n;
    await Promise.all([pass(h), pass(h)]);
    expect(h.calls).toEqual([REMOVE_BOB]);
    // A later pass at the same epoch: still nothing new.
    await pass(h);
    expect(h.calls).toHaveLength(1);
  });

  it("no attempt cap: every commit that drops it again re-requests it again", async () => {
    const h = harness();
    recordOwnRequest(ACCT, GROUP, PROMOTE_BOB, 5n);
    for (let e = 6n; e <= 12n; e++) {
      h.epoch = e;
      await pass(h);
    }
    expect(h.calls).toHaveLength(7);
  });
});

describe("the driver stops by state", () => {
  it("landed: cleared silently, no Not applied row", async () => {
    const h = harness();
    recordOwnRequest(ACCT, GROUP, REMOVE_BOB, 5n);
    h.epoch = 6n;
    h.over = { roster: [{ leafIndex: 0, signatureKeyB64: ALICE_LEAF, accountKeyB64: ALICE }] };
    const summary = await pass(h);
    expect(summary).toEqual({ rerequested: 0, cleared: 1, stopped: 0 });
    expect(loadOwnRequests(ACCT, GROUP).size).toBe(0);
    expect(loadUnapplied(ACCT, GROUP).size).toBe(0);
  });

  it("the subject left: cleared silently", async () => {
    const h = harness();
    recordOwnRequest(ACCT, GROUP, PROMOTE_BOB, 5n);
    h.epoch = 6n;
    h.over = { roster: [{ leafIndex: 0, signatureKeyB64: ALICE_LEAF, accountKeyB64: ALICE }] };
    await pass(h);
    expect(h.calls).toEqual([]);
    expect(loadOwnRequests(ACCT, GROUP).size).toBe(0);
    expect(loadUnapplied(ACCT, GROUP).size).toBe(0);
  });

  it("authority lost (NotPermitted): stops and is listed in Not applied", async () => {
    expect(membershipRefusalDisposition("removeNotOwnerOrAdmin")).toBe("notPermitted");
    const h = harness();
    recordOwnRequest(ACCT, GROUP, REMOVE_BOB, 5n);
    h.epoch = 6n;
    h.fail = refusal("removeNotOwnerOrAdmin");
    const summary = await pass(h);
    expect(summary.stopped).toBe(1);
    expect(loadOwnRequests(ACCT, GROUP).size).toBe(0);
    expect([...loadUnapplied(ACCT, GROUP).keys()]).toEqual([`remove:${BOB}`]);
  });

  it("a DoNotRetry refusal stops it the same way", async () => {
    expect(membershipRefusalDisposition("deviceLimitReached")).toBe("doNotRetry");
    const h = harness();
    const ADMIT: OwnRequest = {
      kind: "admit-device",
      subjectKeyB64: DAVE_DEVICE,
      subjectAccountIdB64: "ZGF2ZS1pZA==",
    };
    recordOwnRequest(ACCT, GROUP, ADMIT, 5n);
    h.epoch = 6n;
    h.fail = refusal("deviceLimitReached");
    await pass(h);
    expect([...loadUnapplied(ACCT, GROUP).keys()]).toEqual([`admit-device:${DAVE_DEVICE}`]);
  });

  it("an owner whose leave is refused (NotPermitted) sees it in Not applied", async () => {
    expect(membershipRefusalDisposition("ownerCannotLeave")).toBe("notPermitted");
    const h = harness();
    recordOwnRequest(ACCT, GROUP, LEAVE, 5n);
    h.epoch = 6n;
    h.fail = refusal("ownerCannotLeave");
    await pass(h);
    expect([...loadUnapplied(ACCT, GROUP).keys()]).toEqual([`leave:${ALICE}`]);
  });

  it("RetryLater is left to the next merged commit: kept, not listed", async () => {
    expect(membershipRefusalDisposition("pendingSetFull")).toBe("retryLater");
    const h = harness();
    recordOwnRequest(ACCT, GROUP, REMOVE_BOB, 5n);
    h.epoch = 6n;
    h.fail = refusal("pendingSetFull");
    await pass(h);
    expect(loadOwnRequests(ACCT, GROUP).get(ownRequestKey(REMOVE_BOB))?.epoch).toBe("6");
    expect(loadUnapplied(ACCT, GROUP).size).toBe(0);
    // Not again at the same epoch...
    await pass(h);
    expect(h.calls).toHaveLength(1);
    // ...but at the next commit, which is what drains a full queue.
    h.fail = null;
    h.epoch = 7n;
    await pass(h);
    expect(h.calls).toHaveLength(2);
  });

  it("a failure that is not a refusal (a network error) is kept, not listed", async () => {
    const h = harness();
    recordOwnRequest(ACCT, GROUP, ADD_DAVE, 5n);
    h.epoch = 6n;
    h.fail = new Error("fetch failed");
    await pass(h);
    expect(loadOwnRequests(ACCT, GROUP).size).toBe(1);
    expect(loadUnapplied(ACCT, GROUP).size).toBe(0);
  });

  it("an approved join with nothing retained stops and is listed", async () => {
    const h = harness();
    recordOwnRequest(ACCT, GROUP, APPROVE_CAROL, 5n);
    h.epoch = 6n;
    await pass(h);
    expect(h.calls).toEqual([]);
    expect([...loadUnapplied(ACCT, GROUP).keys()]).toEqual([`approve-join:${CAROL_LEAF}`]);
  });
});

describe("the store", () => {
  it("a newer role-change intent replaces the older one", () => {
    recordOwnRequest(ACCT, GROUP, PROMOTE_BOB, 5n);
    recordOwnRequest(ACCT, GROUP, TRANSFER_TO_BOB, 6n);
    const all = [...loadOwnRequests(ACCT, GROUP).values()];
    expect(all.map((r) => r.request)).toEqual([TRANSFER_TO_BOB]);
  });

  it("drops a malformed record rather than re-requesting against the wrong subject", () => {
    globalThis.localStorage.setItem(
      `lakko.own-requests.v1.${ACCT}.${GROUP}`,
      JSON.stringify({
        a: { request: { kind: "remove" }, epoch: "5" },
        b: { request: { kind: "set-admin", subjectKeyB64: BOB }, epoch: "5" },
        c: { request: REMOVE_BOB, epoch: "not-a-number" },
        d: { request: REMOVE_BOB, epoch: "5" },
      }),
    );
    expect([...loadOwnRequests(ACCT, GROUP).values()].map((r) => r.request)).toEqual([
      REMOVE_BOB,
    ]);
  });
});

// ---------------------------------------------------------------------------
// 3. The re-issue: re-derived from the intent, against the CURRENT group
// ---------------------------------------------------------------------------

function hostOver(
  members: Array<{ leafIndex: number; signaturePublicKey: Uint8Array; accountKey?: Uint8Array }>,
): { host: ReissueHost; calls: unknown[][] } {
  const calls: unknown[][] = [];
  const proposed = async (...args: unknown[]) => {
    calls.push(args);
    return { kind: "proposed" as const, proposalMessageId: 9 };
  };
  const contacts = {
    addMemberToGroup: vi.fn(async (...a: unknown[]) => proposed("add", ...a.slice(2))),
    removeMemberFromGroup: vi.fn(async (...a: unknown[]) => proposed("remove", a[2])),
    approveExternalJoin: vi.fn(async (...a: unknown[]) => proposed("approve", a[2])),
    setMemberAdmin: vi.fn(async (...a: unknown[]) => proposed("set-admin", ...a.slice(2))),
    transferGroupOwnership: vi.fn(async (...a: unknown[]) =>
      proposed("transfer", ...a.slice(2)),
    ),
    readmitDeviceByKey: vi.fn(async (...a: unknown[]) => proposed("readmit", ...a.slice(2))),
    retireDeviceFromGroup: vi.fn(async (...a: unknown[]) => proposed("retire", a[2])),
  };
  const group = { members: () => members } as unknown as WasmGroup;
  return {
    host: {
      contacts: contacts as unknown as ReissueHost["contacts"],
      identity: {} as unknown as ReissueHost["identity"],
      group,
      markOwnPublished: () => {},
    },
    calls,
  };
}

const b = (s: string): Uint8Array => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

describe("the re-issue re-derives from the intent", () => {
  it("a removal names the subject's CURRENT leaf, found by key, after a leaf was reused", async () => {
    const { host, calls } = hostOver([
      { leafIndex: 0, signaturePublicKey: b(ALICE_LEAF), accountKey: b(ALICE) },
      { leafIndex: 1, signaturePublicKey: b(CAROL_LEAF), accountKey: b(CAROL) },
      { leafIndex: 2, signaturePublicKey: b(BOB_LEAF), accountKey: b(BOB) },
    ]);
    await reissueOwnRequest(host, REMOVE_BOB);
    expect(calls).toEqual([["remove", 2]]);
  });

  it("a removal whose subject has no leaf right now does nothing rather than guess", async () => {
    const { host, calls } = hostOver([
      { leafIndex: 0, signaturePublicKey: b(ALICE_LEAF), accountKey: b(ALICE) },
    ]);
    await reissueOwnRequest(host, REMOVE_BOB);
    expect(calls).toEqual([]);
  });

  /**
   * The role change goes out as its DELTA only — `setMemberAdmin(Bob, true)`
   * — and `setMemberAdmin` recomputes the full table from the table as it is
   * at that moment. A stored full table re-sent here would revert any change
   * that landed between the original request and the re-request.
   */
  it("a role change is re-sent as its delta, never as a table", async () => {
    const { host, calls } = hostOver([]);
    await reissueOwnRequest(host, PROMOTE_BOB);
    await reissueOwnRequest(host, TRANSFER_TO_BOB);
    expect(calls).toHaveLength(2);
    expect(calls[0]?.[0]).toBe("set-admin");
    expect(calls[0]?.[1]).toEqual(b(BOB));
    expect(calls[0]?.[2]).toBe(true);
    expect(calls[1]?.[0]).toBe("transfer");
    expect(calls[1]?.[1]).toEqual(b(BOB));
    expect(calls[1]?.[2]).toBe("admin");
  });

  it("an add re-fetches through the user path (account id + key), never old bytes", async () => {
    const { host, calls } = hostOver([]);
    await reissueOwnRequest(host, ADD_DAVE);
    expect(calls).toEqual([["add", b("ZGF2ZS1pZA=="), b(DAVE)]]);
  });

  it("a device admission re-fetches the device's current KeyPackage", async () => {
    const { host, calls } = hostOver([]);
    await reissueOwnRequest(host, {
      kind: "admit-device",
      subjectKeyB64: DAVE_DEVICE,
      subjectAccountIdB64: "ZGF2ZS1pZA==",
    });
    expect(calls).toEqual([["readmit", b("ZGF2ZS1pZA=="), b(DAVE_DEVICE)]]);
  });
});
