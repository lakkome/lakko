// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 on the receive path (`ChatClient.pollMessages`):
 *
 *   - ADR-0057 obligation 6, no heal on apply: a polled batch that moved the
 *     epoch registers NOTHING (only the root and registrars register, and the
 *     committer did before publishing); a plain member waits;
 *   - contract point 7, members read knocks: an owner / admin also reads the
 *     group's knock queue and feeds each row to the same receive path, which
 *     turns an external-join proposal into the `externalJoinRequest` event
 *     the members panel lists; a plain member does not read it;
 *   - the knock cursor is the signed-in account's: with no account bound the
 *     queue is not read at all, and one account's progress is not another's.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ChatClient } from "./client";
import { bindContactsAccount } from "../contacts/local-store";
import { resetKnockReadSlotsForTests } from "./knock-cursor";
import type { DsClient } from "../ds/client";
import { fakeAccessSigner } from "../ds/access-test-doubles";
import {
  Role,
  type Group as WasmGroup,
  type Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xd5, nextGid]);
}

/** Payload byte 0 says what a row is: 0xc0 a commit (moves the epoch), 0x1b a
 *  knock proposal, 0xa7 an attribution (not an MLS message). */
/**
 * `rank` is the ADR-0045 committer rank and `role` the ADR-0022 role. They
 * default together (owner = rank 0, no role = no rank) because they agreed
 * until ADR-0050 section 2.5a gave a plain member a rank while it holds a
 * device admission; the knock gate must follow the ROLE.
 */
function fakeGroup(
  rank: number | undefined,
  role: Role | undefined = rank === undefined ? undefined : rank === 0 ? Role.Owner : Role.Admin,
): { group: WasmGroup; epoch: () => bigint } {
  let epoch = 5n;
  const gid = freshGid();
  const group = {
    get groupId(): Uint8Array {
      return gid;
    },
    get epoch(): bigint {
      return epoch;
    },
    dsAccessSigner: fakeAccessSigner(() => epoch),
    ownCommitterRank: () => rank,
    ownRole: () => role,
    members: () => [],
    processIncomingVerifyingRoles: vi.fn((_id: unknown, payload: Uint8Array) => {
      if (payload[0] === 0xc0) {
        epoch += 1n;
        return { kind: "Commit" };
      }
      if (payload[0] === 0x1b) {
        return {
          kind: "ExternalJoinProposal",
          externalJoinSignatureKey: new Uint8Array(32).fill(payload[1] ?? 0),
          externalJoinRetained: true,
        };
      }
      throw new Error("not an MLS message");
    }),
    // A faithful double of the core's knock gate: anything that is not a
    // stranger's external-join proposal is refused BEFORE processing (so a
    // commit here does NOT move the epoch); a proposal is processed like the
    // journal path would.
    processKnock: vi.fn((_id: unknown, payload: Uint8Array) => {
      if (payload[0] !== 0x1b) throw new Error("not a stranger's external-join proposal");
      return {
        kind: "ExternalJoinProposal",
        externalJoinSignatureKey: new Uint8Array(32).fill(payload[1] ?? 0),
        externalJoinRetained: true,
      };
    }),
  } as unknown as WasmGroup;
  return { group, epoch: () => epoch };
}

function row(id: number, bytes: number[]): { id: number; payload: Uint8Array; createdAt: string } {
  return { id, payload: new Uint8Array(bytes), createdAt: "t" };
}

function fakeDs(
  journal: ReturnType<typeof row>[],
  knocks: ReturnType<typeof row>[],
): { ds: DsClient; registrations: string[]; knockReads: number[] } {
  const registrations: string[] = [];
  const knockReads: number[] = [];
  const ds = {
    fetchGroup: vi.fn(async () => ({ messages: journal, hasMore: false })),
    fetchKnocks: vi.fn(async (_g: Uint8Array, opts: { after?: number }) => {
      knockReads.push(opts.after ?? 0);
      return { messages: knocks, hasMore: false };
    }),
    registerGroupEpoch: vi.fn(async (_g: Uint8Array, epoch: bigint) => {
      registrations.push(`${epoch}`);
    }),
  } as unknown as DsClient;
  return { ds, registrations, knockReads };
}

function chat(ds: DsClient): ChatClient {
  return new ChatClient(
    ds,
    { identityProvider: () => IDENTITY_TOKEN },
  );
}

/** Let anything fire-and-forget settle. */
const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

/** The signed-in account the knock cursor is recorded for. */
const ACCOUNT_A = "YWNjb3VudC1hLTAwMDAwMA==";
const ACCOUNT_B = "YWNjb3VudC1iLTAwMDAwMA==";

/** A working in-memory `localStorage` (the node global has no `getItem`). */
function installLocalStorage(): void {
  const data = new Map<string, string>();
  (globalThis as unknown as { localStorage: Storage }).localStorage = {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k: string) => data.get(k) ?? null,
    key: (i: number) => [...data.keys()][i] ?? null,
    removeItem: (k: string) => void data.delete(k),
    setItem: (k: string, v: string) => void data.set(k, String(v)),
  };
}

beforeEach(() => {
  installLocalStorage();
  bindContactsAccount(ACCOUNT_A);
});

afterEach(() => {
  bindContactsAccount(null);
  vi.clearAllMocks();
});

describe("ADR-0057 obligation 6 — plain members no longer heal", () => {
  it("a batch that moved the epoch registers nothing: the committer already did", async () => {
    const g = fakeGroup(undefined);
    const { ds, registrations } = fakeDs([row(1, [0xc0]), row(2, [0xc0])], []);

    await chat(ds).pollMessages(null, g.group, 0);
    await settle();

    expect(g.epoch()).toBe(7n);
    expect(registrations).toEqual([]);
  });

  it("a batch that did not move the epoch registers nothing", async () => {
    const g = fakeGroup(undefined);
    const { ds, registrations } = fakeDs([row(1, [0xa7])], []);

    await chat(ds).pollMessages(null, g.group, 0);
    await settle();

    expect(registrations).toEqual([]);
  });
});

describe("ADR-0053 contract point 7 — an owner or admin reads the knock queue", () => {
  it("a knocked external-join proposal surfaces as an `externalJoinRequest` event; the attribution beside it is skipped", async () => {
    const g = fakeGroup(0);
    const { ds, knockReads } = fakeDs([], [row(1, [0xa7]), row(2, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    expect(knockReads).toHaveLength(1);
    const requests = result.events.filter((e) => e.kind === "externalJoinRequest");
    expect(requests).toHaveLength(1);
    expect(requests[0]).toMatchObject({
      kind: "externalJoinRequest",
      joinerSignatureKey: new Uint8Array(32).fill(0x42),
    });
  });

  it("a member's COMMIT posted as a knock goes through the knock gate, is refused and never applied", async () => {
    const g = fakeGroup(0);
    const { ds } = fakeDs([], [row(1, [0xc0]), row(2, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    // The commit did not move this owner's epoch: it was never processed.
    expect(g.epoch()).toBe(5n);
    const journalApply = (g.group as unknown as { processIncomingVerifyingRoles: ReturnType<typeof vi.fn> })
      .processIncomingVerifyingRoles;
    expect(journalApply).not.toHaveBeenCalled();
    const knockApply = (g.group as unknown as { processKnock: ReturnType<typeof vi.fn> }).processKnock;
    expect(knockApply).toHaveBeenCalledTimes(2);
    // The proposal beside it still surfaces.
    expect(result.events.filter((e) => e.kind === "externalJoinRequest")).toHaveLength(1);
  });

  it("a plain member does not read knocks", async () => {
    const g = fakeGroup(undefined);
    const { ds, knockReads } = fakeDs([], [row(1, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    expect(knockReads).toEqual([]);
    expect(result.events).toEqual([]);
  });

  it("an admin reads the knock queue", async () => {
    const g = fakeGroup(1, Role.Admin);
    const { ds, knockReads } = fakeDs([], [row(1, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    expect(knockReads).toHaveLength(1);
    expect(result.events.filter((e) => e.kind === "externalJoinRequest")).toHaveLength(1);
  });

  // ADR-0050 section 2.5a — a plain member holds a committer rank while its
  // pending store holds a device admission it could sweep. That rank is not
  // the authority to approve a join, so it must not open the knock queue.
  it("a plain member holding a committer rank (a device admission to sweep) does not read knocks", async () => {
    const g = fakeGroup(3, Role.Member);
    const { ds, knockReads } = fakeDs([], [row(1, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    expect(knockReads).toEqual([]);
    expect(result.events).toEqual([]);
  });

  it("with no signed-in account the knock queue is not read, so nothing is re-fed", async () => {
    bindContactsAccount(null);
    const g = fakeGroup(0);
    const { ds, knockReads } = fakeDs([], [row(1, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    expect(knockReads).toEqual([]);
    expect(result.events).toEqual([]);
  });

  it("one account's knock progress is not another's on the same browser", async () => {
    const g = fakeGroup(0);
    const first = fakeDs([], [row(4, [0x1b, 0x42])]);
    await chat(first.ds).pollMessages(null, g.group, 0);
    expect(first.knockReads).toEqual([0]);

    // Account B signs in on the same browser and administers the same group:
    // it starts from its own cursor, not from where A stopped.
    bindContactsAccount(ACCOUNT_B);
    resetKnockReadSlotsForTests();
    const second = fakeDs([], [row(4, [0x1b, 0x43])]);
    const result = await chat(second.ds).pollMessages(null, g.group, 0);
    expect(second.knockReads).toEqual([0]);
    expect(result.events.filter((e) => e.kind === "externalJoinRequest")).toHaveLength(1);

    // And A, signed in again, resumes after the row it consumed.
    bindContactsAccount(ACCOUNT_A);
    resetKnockReadSlotsForTests();
    const third = fakeDs([], []);
    await chat(third.ds).pollMessages(null, g.group, 0);
    expect(third.knockReads).toEqual([4]);
  });
});
