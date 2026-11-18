// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 1 through the real poll (`ChatClient.pollMessages`): the
 * registered commit that fails is not skipped and is retried; any other
 * failing row is skipped as before. Phase 1b: the DS names the registered
 * commit rows (`registeredCommits`), and a group loaded below the epoch
 * recorded with its cursor reads again from the commit leaving it.
 *
 * The double is `receive-durability.test.ts`'s: a JSON wire whose rows say
 * what they are, and a group whose epoch moves only when a commit applies.
 * The two framing readers the poll calls on a failed row are mocked to read
 * the same JSON, since the node environment has no MLS bytes to frame.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./commit-outbox", () => ({
  markAcked: async () => {},
  loadUnacked: async () => [],
}));

vi.mock("../../wasm/lakko_ffi_wasm", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  const read = (bytes: Uint8Array): Row | undefined => {
    try {
      return JSON.parse(new TextDecoder().decode(bytes)) as Row;
    } catch {
      return undefined;
    }
  };
  return {
    ...actual,
    epochOfMessage: (bytes: Uint8Array) => {
      const row = read(bytes);
      return row === undefined ? undefined : BigInt(row.sealed);
    },
    epochAfterMessage: (bytes: Uint8Array) => {
      const row = read(bytes);
      if (row === undefined) return undefined;
      return BigInt(row.t === "commit" ? row.sealed + 1 : row.sealed);
    },
  };
});

import { ChatClient } from "./client";
import { readCursorEpoch, recordCursorEpoch } from "./cursor-epoch";
import { bindContactsAccount } from "../contacts/local-store";
import { REGISTERED_COMMIT_MAX_ATTEMPTS, registeredCommitHolds } from "./registered-commit-hold";
import type { DsClient } from "../ds/client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

type Row =
  /** A commit framed at `sealed`; applies only while `applies()` says so. */
  | { readonly t: "commit"; readonly sealed: number }
  /** An application message framed at `sealed`. */
  | { readonly t: "app"; readonly sealed: number };

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;
let nextGroupId = 0x31;

function device(opts: { latestEpoch: number | undefined }) {
  /** Epoch -> the row the DS registered as reaching it (ADR-0081). */
  const registered = new Map<bigint, number>();
  nextGroupId += 1;
  const groupId = new Uint8Array([nextGroupId, 0x91]);
  let epoch = 0n;
  let commitApplies = false;
  let cursor = 0;
  let nextId = 100;
  let fetches = 0;
  const journal: { id: number; payload: Uint8Array; createdAt: string }[] = [];
  const group = {
    get groupId(): Uint8Array {
      return groupId;
    },
    get epoch(): bigint {
      return epoch;
    },
    get memberCount(): number {
      return 2;
    },
    members: () => [],
    processIncomingVerifyingRoles: (_id: unknown, bytes: Uint8Array): unknown => {
      const row = JSON.parse(new TextDecoder().decode(bytes)) as Row;
      if (BigInt(row.sealed) !== epoch) throw new Error("ValidationError(WrongEpoch)");
      if (row.t === "commit") {
        if (!commitApplies) throw new Error("InvalidCommit(MissingProposal)");
        epoch += 1n;
        return { kind: "Commit" };
      }
      return {
        kind: "Proposal",
        proposal: { kind: "remove", stored: true, senderLeaf: 1, addSignatureKey: undefined, removeTargetLeaf: 2 },
      };
    },
  } as unknown as WasmGroup;
  const ds = {
    fetchGroup: async (_gid: Uint8Array, o: { after: number }) => {
      fetches += 1;
      return {
        messages: journal.filter((r) => r.id > o.after),
        hasMore: false,
        ...(opts.latestEpoch !== undefined
          ? { latestEpoch: opts.latestEpoch, registeredCommits: new Map(registered) }
          : {}),
      };
    },
  } as unknown as DsClient;
  const chat = new ChatClient(ds, { identityProvider: () => IDENTITY_TOKEN, onPersist: async () => {} });
  return {
    group,
    chat,
    cursorRef: { read: () => cursor, advance: (to: number) => void (cursor = to) },
    cursor: () => cursor,
    epoch: () => epoch,
    fetches: () => fetches,
    letCommitApply: () => {
      commitApplies = true;
    },
    /** Roll the group back (a stale snapshot written over a newer one). */
    rollBackTo: (to: bigint) => {
      epoch = to;
    },
    /** Append a row; a commit is registered with the DS unless `garbage`
     *  (a commit-shaped row posted through the plain publish route). */
    push: (row: Row, garbage = false) => {
      nextId += 1;
      journal.push({
        id: nextId,
        payload: new TextEncoder().encode(JSON.stringify(row)),
        createdAt: "2026-11-18T12:00:00Z",
      });
      if (row.t === "commit" && !garbage) registered.set(BigInt(row.sealed + 1), nextId);
      return nextId;
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(new Date("2026-11-18T12:00:00Z"));
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(() => {
  registeredCommitHolds.clear();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("ADR-0091 phase 1 in the poll", () => {
  it("a registered commit that fails is held, not skipped, and applies on a later poll", async () => {
    const d = device({ latestEpoch: 1 });
    const before = d.push({ t: "app", sealed: 0 });
    d.push({ t: "commit", sealed: 0 });
    d.push({ t: "app", sealed: 1 });

    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    // Stopped at the commit: the cursor stays before it, the row after it is
    // not consumed.
    expect(d.cursor()).toBe(before);
    expect(d.epoch()).toBe(0n);
    expect(d.fetches()).toBe(1);

    // Not due yet: the poll does not fetch.
    await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(d.fetches()).toBe(1);
    expect(d.cursor()).toBe(before);

    // Due, and the state that made it fail has changed.
    vi.setSystemTime(new Date("2026-11-18T12:00:02Z"));
    d.letCommitApply();
    await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(d.fetches()).toBe(2);
    expect(d.epoch()).toBe(1n);
    expect(d.cursor()).toBe(before + 2);
    expect(registeredCommitHolds.heldRowId(btoa(String.fromCharCode(...d.group.groupId)))).toBeUndefined();
  });

  it("a failing row the DS did not register as the next epoch is still skipped", async () => {
    const d = device({ latestEpoch: undefined });
    d.push({ t: "commit", sealed: 0 });
    const last = d.push({ t: "app", sealed: 0 });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.cursor()).toBe(last);
  });

  it("a commit of an earlier epoch is skipped even when the DS is ahead", async () => {
    const d = device({ latestEpoch: 5 });
    d.letCommitApply();
    d.push({ t: "commit", sealed: 0 });
    d.push({ t: "commit", sealed: 0 }, true); // already past: framed below the new epoch
    const last = d.push({ t: "app", sealed: 1 });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.epoch()).toBe(1n);
    expect(d.cursor()).toBe(last);
  });

  it("a commit-shaped row the DS did not register is skipped at once; the registered one is held", async () => {
    const d = device({ latestEpoch: 1 });
    const garbage = d.push({ t: "commit", sealed: 0 }, true);
    const real = d.push({ t: "commit", sealed: 0 });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    // Past the garbage at once, stopped before the registered commit.
    expect(d.cursor()).toBe(garbage);
    expect(real).toBe(garbage + 1);
    vi.setSystemTime(new Date("2026-11-18T12:00:02Z"));
    d.letCommitApply();
    await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(d.epoch()).toBe(1n);
    expect(d.cursor()).toBe(real);
  });

  it("after the last attempt the commit is skipped and the poll moves on", async () => {
    const d = device({ latestEpoch: 1 });
    const commit = d.push({ t: "commit", sealed: 0 });
    const last = d.push({ t: "app", sealed: 0 });
    for (let attempt = 1; attempt <= REGISTERED_COMMIT_MAX_ATTEMPTS; attempt += 1) {
      await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
      vi.setSystemTime(Date.now() + 301_000);
    }
    expect(d.cursor()).toBe(last);
    expect(commit).toBeLessThan(last);
    expect(d.epoch()).toBe(0n);
  });
});

describe("ADR-0091 phase 1b: a snapshot rolled back under its cursor", () => {
  const ACCOUNT = "Y2FyYS1hY2NvdW50LTAwMDA=";
  let data: Map<string, string>;

  beforeEach(() => {
    data = new Map();
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
    bindContactsAccount(ACCOUNT);
  });

  afterEach(() => bindContactsAccount(null));

  it("records the epoch with the cursor, and reads again from the commit leaving a rolled-back epoch", async () => {
    const d = device({ latestEpoch: 1 });
    const gid = btoa(String.fromCharCode(...d.group.groupId));
    d.letCommitApply();
    d.push({ t: "app", sealed: 0 });
    const commit = d.push({ t: "commit", sealed: 0 });
    const last = d.push({ t: "app", sealed: 1 });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(d.cursor()).toBe(last);
    expect(d.epoch()).toBe(1n);
    expect(readCursorEpoch(gid)).toBe(1n);

    // An older snapshot is put back: the group is at 0, the cursor past the
    // commit that left 0.
    d.rollBackTo(0n);
    const result = await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(d.epoch()).toBe(1n);
    // The cursor never moves backward.
    expect(d.cursor()).toBe(last);
    expect(result.cursor).toBe(last);
    expect(commit).toBeLessThan(last);
    // The row after the commit was delivered again.
    expect(result.events.length).toBeGreaterThan(0);
  });

  it("does not rewind a group at its recorded epoch", async () => {
    const d = device({ latestEpoch: 1 });
    const gid = btoa(String.fromCharCode(...d.group.groupId));
    d.letCommitApply();
    d.push({ t: "commit", sealed: 0 });
    const last = d.push({ t: "app", sealed: 1 });
    await d.chat.pollMessages(null, d.group, 0, 0, d.cursorRef);
    expect(readCursorEpoch(gid)).toBe(1n);
    const again = await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(again.events).toEqual([]);
    expect(d.cursor()).toBe(last);
    // A record above the group with no commit named for its epoch: nothing.
    recordCursorEpoch(gid, 5n);
    const none = await d.chat.pollMessages(null, d.group, d.cursor(), 0, d.cursorRef);
    expect(none.events).toEqual([]);
  });
});
