// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sweep backoff after a refused staged-epoch registration
 * (`sweep-backoff.ts`): the pure schedule, the per-group state, and the
 * adapter in `contacts/client.ts` driving it over a fake group whose
 * registration the DS refuses.
 *
 * Before it, a refused registration (`401` in a journal nobody had claimed)
 * was retried at the tail of every poll, every ~5 s, forever (the live walk of
 * 2026-09-29).
 *
 * Discrimination (run): with the `mayStartSweep` gate disabled in
 * `runCommitterSweep`, two tests fail - "a refused registration backs off the
 * next sweep" and "the owner's claim landing ends the wait" - because the
 * sweep inside the wait stages and registers again instead of reporting
 * `backing-off`.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

vi.mock("./commit-outbox", () => ({
  markAcked: vi.fn(async () => {}),
  loadUnacked: vi.fn(async () => [] as unknown[]),
  deleteEntry: vi.fn(async () => {}),
}));

import init from "../../wasm/lakko_ffi_wasm";
import { ContactsClient } from "../contacts/client";
import { dsAccess } from "../ds/access";
import { fakeAccessSigner, fakeRootAuthority } from "../ds/access-test-doubles";
import { DsHttpError, type DsClient } from "../ds/client";
import { NoJournalAuthorityError } from "../ds/ownership";
import type { DiscoveryClient } from "../discovery/client";
import type { ActingCommitter } from "./committer-beacon";
import {
  SWEEP_BACKOFF_INITIAL_MS,
  SWEEP_BACKOFF_MAX_MS,
  SweepBackoff,
  isBackoffRefusal,
  nextSweepBackoffMs,
  sweepBackoff,
} from "./sweep-backoff";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  // `sweepIfCommitter` loads the wasm module; initialise it from disk.
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("the schedule", () => {
  it("30 s, doubling, capped at 10 minutes", () => {
    const seen: number[] = [];
    let d: number | null = null;
    for (let i = 0; i < 8; i++) {
      d = nextSweepBackoffMs(d);
      seen.push(d);
    }
    expect(SWEEP_BACKOFF_INITIAL_MS).toBe(30_000);
    expect(SWEEP_BACKOFF_MAX_MS).toBe(600_000);
    expect(seen).toEqual([30_000, 60_000, 120_000, 240_000, 480_000, 600_000, 600_000, 600_000]);
  });

  it("which refusals count", () => {
    expect(isBackoffRefusal(new DsHttpError(401, "/access", ""))).toBe(true);
    expect(isBackoffRefusal(new DsHttpError(403, "/access", ""))).toBe(true);
    expect(isBackoffRefusal(new DsHttpError(400, "/access", ""))).toBe(true);
    expect(isBackoffRefusal(new NoJournalAuthorityError())).toBe(true);
    // A lost race and a network failure resolve on their own.
    expect(isBackoffRefusal(new DsHttpError(409, "/access", ""))).toBe(false);
    expect(isBackoffRefusal(new Error("offline"))).toBe(false);
  });
});

describe("SweepBackoff", () => {
  it("waits, doubles, and resets on success", () => {
    const b = new SweepBackoff();
    expect(b.mayStart("g", 0, "fp")).toBe(true);
    expect(b.noteRefused("g", 0, "fp")).toBe(30_000);
    expect(b.mayStart("g", 29_999, "fp")).toBe(false);
    expect(b.mayStart("g", 30_000, "fp")).toBe(true);
    expect(b.noteRefused("g", 30_000, "fp")).toBe(60_000);
    expect(b.mayStart("g", 89_999, "fp")).toBe(false);
    b.reset("g");
    expect(b.mayStart("g", 89_999, "fp")).toBe(true);
    expect(b.noteRefused("g", 90_000, "fp")).toBe(30_000);
  });

  it("a membership or role change ends the wait and restarts the schedule", () => {
    const b = new SweepBackoff();
    b.noteRefused("g", 0, "fp");
    b.noteRefused("g", 30_000, "fp");
    expect(b.mayStart("g", 31_000, "fp")).toBe(false);
    expect(b.mayStart("g", 31_000, "fp2")).toBe(true);
    expect(b.noteRefused("g", 31_000, "fp2")).toBe(30_000);
  });

  it("groups are independent", () => {
    const b = new SweepBackoff();
    b.noteRefused("a", 0, "fp");
    expect(b.mayStart("b", 1, "fp")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Through the sweep adapter
// ---------------------------------------------------------------------------

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;
const CURSOR = { read: (): number => 0, advance: (): void => {} };
const ACT = async (): Promise<ActingCommitter> => ({ kind: "act" });
const acct = (n: number): Uint8Array => new Uint8Array(32).fill(n);

let seq = 0;
function sweepingGroup(): { group: WasmGroup; order: string[]; addMember: () => void } {
  seq += 1;
  const gid = new Uint8Array(32).fill(0x90 + seq);
  const order: string[] = [];
  let staged = false;
  const members = [
    { leafIndex: 0, signaturePublicKey: acct(0xa0), accountKey: acct(1) },
    { leafIndex: 1, signaturePublicKey: acct(0xa1), accountKey: acct(2) },
  ];
  const group = {
    get groupId(): Uint8Array {
      return gid;
    },
    get epoch(): bigint {
      return 6n;
    },
    dsAccessSigner: fakeAccessSigner(() => 6n),
    dsStagedAccessSigner: () => (staged ? fakeAccessSigner(() => 7n)(undefined) : undefined),
    roleTableOwner: () => acct(1),
    roleTableAdmins: () => new Uint8Array(0),
    ownCommitterRank: () => 0,
    members: () => members,
    pendingProposals: () => [{ kind: "add" }],
    hasStagedSweep: () => staged,
    stagePendingProposals: () => {
      order.push("stage");
      staged = true;
      return { commit: new Uint8Array([0x5e]), welcome: undefined, swept: () => [] };
    },
    mergeStagedSweep: () => {
      order.push("merge");
      staged = false;
    },
    abandonStagedSweep: () => {
      order.push("abandon");
      staged = false;
    },
    processIncomingVerifyingRoles: () => ({ kind: "Commit" }),
  } as unknown as WasmGroup;
  return {
    group,
    order,
    addMember: () => members.push({ leafIndex: 2, signaturePublicKey: acct(0xa2), accountKey: acct(3) }),
  };
}

function client(register: () => Promise<void>): { contacts: ContactsClient; registrations: () => number } {
  let n = 0;
  let id = 100;
  const ds = {
    registerGroupEpoch: vi.fn(async () => {
      n += 1;
      await register();
    }),
    publishGroup: vi.fn(async () => ({ id: (id += 1), createdAt: "t" })),
    publishMailbox: vi.fn(async () => ({ id: (id += 1), createdAt: "t" })),
    fetchGroup: vi.fn(async () => ({ messages: [], hasMore: false })),
  } as unknown as DsClient;
  const contacts = new ContactsClient({} as unknown as DiscoveryClient, ds, {
    identityProvider: () => IDENTITY_TOKEN,
    onPersist: async () => {},
    onPersistWithCommit: async () => {},
  });
  return { contacts, registrations: () => n };
}

let offOwner: (() => void) | null = null;
afterEach(() => {
  offOwner?.();
  offOwner = null;
  dsAccess.setAuthorityFactory(null);
  sweepBackoff.clear();
  vi.restoreAllMocks();
});

function asOwner(): void {
  offOwner = dsAccess.registerOwnerSource(() => true);
  dsAccess.setAuthorityFactory({
    root: () => fakeRootAuthority("root"),
    registrar: () => ({ kind: "none" }),
  });
}

describe("the sweep adapter backs off a refused registration", () => {
  it("a refused registration backs off the next sweep; the staged sweep is still abandoned", async () => {
    asOwner();
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000_000);
    const g = sweepingGroup();
    const { contacts, registrations } = client(async () => {
      throw new DsHttpError(401, "/access", "proof required");
    });
    const sweep = () => contacts.sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});

    await expect(sweep()).rejects.toBeInstanceOf(DsHttpError);
    // ADR-0045 obligation 1 unchanged: nothing is left staged.
    expect(g.order).toEqual(["stage", "abandon"]);

    // Five seconds later (the next poll tail): not staged again.
    now.mockReturnValue(1_005_000);
    expect(await sweep()).toEqual({ kind: "backing-off", rank: 0 });
    expect(g.order).toEqual(["stage", "abandon"]);
    expect(registrations()).toBe(1);

    // After the wait: tried again, refused again, and the wait doubles.
    now.mockReturnValue(1_030_000);
    await expect(sweep()).rejects.toBeInstanceOf(DsHttpError);
    expect(registrations()).toBe(2);
    now.mockReturnValue(1_030_000 + 59_999);
    expect((await sweep()).kind).toBe("backing-off");
    now.mockReturnValue(1_030_000 + 60_000);
    await expect(sweep()).rejects.toBeInstanceOf(DsHttpError);
    expect(registrations()).toBe(3);
  });

  it("a membership change ends the wait", async () => {
    asOwner();
    const now = vi.spyOn(Date, "now").mockReturnValue(2_000_000);
    const g = sweepingGroup();
    let refuse = true;
    const { contacts } = client(async () => {
      if (refuse) throw new DsHttpError(403, "/access", "");
    });
    const sweep = () => contacts.sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});
    await expect(sweep()).rejects.toBeInstanceOf(DsHttpError);
    now.mockReturnValue(2_001_000);
    g.addMember();
    refuse = false;
    expect((await sweep()).kind).toBe("committed");
  });

  it("the owner's claim landing ends the wait (reset)", async () => {
    asOwner();
    const now = vi.spyOn(Date, "now").mockReturnValue(3_000_000);
    const g = sweepingGroup();
    let refuse = true;
    const { contacts } = client(async () => {
      if (refuse) throw new DsHttpError(401, "/access", "");
    });
    const sweep = () => contacts.sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});
    await expect(sweep()).rejects.toBeInstanceOf(DsHttpError);
    now.mockReturnValue(3_001_000);
    expect((await sweep()).kind).toBe("backing-off");
    // What the ownership provider does on `dsAccess.onOwnerClaimed`.
    sweepBackoff.reset(btoa(String.fromCharCode(...g.group.groupId)));
    refuse = false;
    expect((await sweep()).kind).toBe("committed");
  });

  it("a lost race (409) is not backed off", async () => {
    asOwner();
    vi.spyOn(Date, "now").mockReturnValue(4_000_000);
    const g = sweepingGroup();
    let refuse = true;
    const { contacts, registrations } = client(async () => {
      if (refuse) throw new DsHttpError(409, "/access", "");
    });
    const sweep = () => contacts.sweepIfCommitter(null, g.group, CURSOR, () => {}, ACT, () => {});
    await expect(sweep()).rejects.toBeInstanceOf(DsHttpError);
    refuse = false;
    expect((await sweep()).kind).toBe("committed");
    expect(registrations()).toBe(2);
  });
});
