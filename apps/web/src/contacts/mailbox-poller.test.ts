// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The identity-mailbox poll under overlap (`mailbox-poller.ts`): a background
 * long poll and a user-initiated refresh fetching the same rows must dispatch
 * them exactly once, one batch at a time, and a failed dispatch must hold the
 * cursor rather than skip the rows.
 */
import { describe, expect, it } from "vitest";

import {
  createMailboxPoller,
  MailboxDispatchHeld,
  type MailboxPollerDeps,
} from "./mailbox-poller";

interface Row {
  readonly id: number;
}

/** A deferred promise, so a test decides when each fetch returns. */
function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function journal(rows: readonly number[]) {
  return (after: number) => {
    const events = rows.filter((id) => id > after).map((id) => ({ id }));
    const last = events[events.length - 1];
    return { events, cursor: last === undefined ? after : last.id };
  };
}

describe("createMailboxPoller", () => {
  it("dispatches a batch fetched twice concurrently exactly once", async () => {
    let cursor = 0;
    const dispatched: number[] = [];
    const read = journal([1, 2, 3]);
    const fetches: Array<ReturnType<typeof deferred<void>>> = [];
    const deps: MailboxPollerDeps<Row> = {
      readCursor: () => cursor,
      writeCursor: (c) => {
        cursor = c;
      },
      fetch: async (after) => {
        const gate = deferred<void>();
        fetches.push(gate);
        await gate.promise;
        return read(after);
      },
      dispatch: async (events) => {
        for (const e of events) dispatched.push(e.id);
      },
    };
    const poller = createMailboxPoller(deps);
    // The long poll and the Refresh button both start from cursor 0.
    const background = poller.poll(25);
    const manual = poller.poll(0);
    expect(fetches).toHaveLength(2);
    fetches[1]?.resolve();
    fetches[0]?.resolve();
    const outcomes = await Promise.all([background, manual]);
    expect(dispatched).toEqual([1, 2, 3]);
    expect(outcomes.sort()).toEqual(["cursor-moved", "dispatched"]);
    expect(cursor).toBe(3);
  });

  it("serializes dispatch: a second batch waits for the first to finish", async () => {
    let cursor = 0;
    const log: string[] = [];
    const firstDispatch = deferred<void>();
    let fetchCount = 0;
    const deps: MailboxPollerDeps<Row> = {
      readCursor: () => cursor,
      writeCursor: (c) => {
        cursor = c;
      },
      fetch: async (after) => {
        fetchCount += 1;
        // The second fetch is made from the cursor the first will write, as a
        // real re-armed poll would be.
        return fetchCount === 1
          ? { events: [{ id: 1 }], cursor: 1 }
          : { events: [{ id: 2 }], cursor: Math.max(after, 2) };
      },
      dispatch: async (events) => {
        const id = events[0]?.id;
        log.push(`start ${id}`);
        if (id === 1) await firstDispatch.promise;
        log.push(`end ${id}`);
      },
    };
    const poller = createMailboxPoller(deps);
    const one = poller.poll(0);
    await Promise.resolve();
    await Promise.resolve();
    // Pretend the second poll fetched from 1 (as it would after the first).
    cursor = 1;
    const two = poller.poll(0);
    cursor = 0;
    await Promise.resolve();
    await Promise.resolve();
    expect(log).toEqual(["start 1"]);
    firstDispatch.resolve();
    await one;
    await two;
    // The second batch was fetched from 1 but the cursor is 1 only after the
    // first dispatch wrote it, so it applies — strictly after "end 1".
    expect(log.indexOf("end 1")).toBeLessThan(log.indexOf("start 2"));
  });

  it("holds the cursor when the dispatch throws, and re-fetches the same rows", async () => {
    let cursor = 5;
    const read = journal([6, 7]);
    let fail = true;
    const seen: number[][] = [];
    const deps: MailboxPollerDeps<Row> = {
      readCursor: () => cursor,
      writeCursor: (c) => {
        cursor = c;
      },
      fetch: async (after) => read(after),
      dispatch: async (events) => {
        seen.push(events.map((e) => e.id));
        if (fail) throw new Error("persist failed");
      },
    };
    const poller = createMailboxPoller(deps);
    await expect(poller.poll(0)).rejects.toBeInstanceOf(MailboxDispatchHeld);
    expect(cursor).toBe(5);
    fail = false;
    await expect(poller.poll(0)).resolves.toBe("dispatched");
    expect(cursor).toBe(7);
    expect(seen).toEqual([
      [6, 7],
      [6, 7],
    ]);
  });

  it("advances past an empty batch without dispatching and never winds backward", async () => {
    let cursor = 9;
    let dispatchCalls = 0;
    const poller = createMailboxPoller<Row>({
      readCursor: () => cursor,
      writeCursor: (c) => {
        cursor = c;
      },
      fetch: async () => ({ events: [], cursor: 3 }),
      dispatch: async () => {
        dispatchCalls += 1;
      },
    });
    await expect(poller.poll(0)).resolves.toBe("dispatched");
    expect(dispatchCalls).toBe(0);
    expect(cursor).toBe(9);
  });

  it("a failed fetch rejects without touching the cursor or the dispatch lock", async () => {
    let cursor = 0;
    let fetchFails = true;
    const poller = createMailboxPoller<Row>({
      readCursor: () => cursor,
      writeCursor: (c) => {
        cursor = c;
      },
      fetch: async (after) => {
        if (fetchFails) throw new Error("network");
        return journal([1])(after);
      },
      dispatch: async () => undefined,
    });
    await expect(poller.poll(0)).rejects.toThrow("network");
    fetchFails = false;
    await expect(poller.poll(0)).resolves.toBe("dispatched");
    expect(cursor).toBe(1);
  });
  // One poller serves every session of the page. Two accounts can sit at the
  // same cursor, so the cursor check alone cannot keep A's mail out of B.
  describe("across a change of account", () => {
    interface Inbox {
      readonly account: string;
      cursor: number;
      readonly requests: number[];
    }

    function harness() {
      const state: { current: Inbox | null } = { current: null };
      const gates: Array<ReturnType<typeof deferred<void>>> = [];
      const journals: Record<string, readonly number[]> = { A: [8, 9], B: [8] };
      const poller = createMailboxPoller<Row, Inbox | null>({
        scope: () => state.current,
        readCursor: (inbox) => inbox?.cursor ?? 0,
        writeCursor: (c, inbox) => {
          if (inbox === null) throw new Error("no inbox");
          inbox.cursor = c;
        },
        fetch: async (after, _wait, _signal, inbox) => {
          const gate = deferred<void>();
          gates.push(gate);
          await gate.promise;
          return journal(journals[inbox?.account ?? ""] ?? [])(after);
        },
        dispatch: async (events, inbox) => {
          if (inbox === null) throw new Error("no inbox");
          for (const e of events) inbox.requests.push(e.id);
        },
      });
      return { state, gates, poller };
    }

    it("drops A's batch when B signed in at the same cursor, and leaves B's cursor alone", async () => {
      const { state, gates, poller } = harness();
      const a: Inbox = { account: "A", cursor: 7, requests: [] };
      const b: Inbox = { account: "B", cursor: 7, requests: [] };
      state.current = a;
      const stale = poller.poll(25);
      // Sign out, sign in as B: same cursor value, different mailbox.
      state.current = b;
      gates[0]?.resolve();
      await expect(stale).resolves.toBe("scope-changed");
      expect(b.requests).toEqual([]);
      expect(b.cursor).toBe(7);
      expect(a.requests).toEqual([]);
      expect(a.cursor).toBe(7);

      // B's own poll still reads B's own mail from B's own cursor.
      const own = poller.poll(0);
      gates[1]?.resolve();
      await expect(own).resolves.toBe("dispatched");
      expect(b.requests).toEqual([8]);
      expect(b.cursor).toBe(8);
    });

    it("drops the batch when the account signed out and nobody signed in", async () => {
      const { state, gates, poller } = harness();
      const a: Inbox = { account: "A", cursor: 7, requests: [] };
      state.current = a;
      const stale = poller.poll(25);
      state.current = null;
      gates[0]?.resolve();
      await expect(stale).resolves.toBe("scope-changed");
      expect(a.requests).toEqual([]);
      expect(a.cursor).toBe(7);
    });

    it("writes the cursor of the mailbox the batch was dispatched for, not the current one", async () => {
      const state: { current: Inbox | null } = { current: null };
      const a: Inbox = { account: "A", cursor: 7, requests: [] };
      const b: Inbox = { account: "B", cursor: 7, requests: [] };
      const inDispatch = deferred<void>();
      const finish = deferred<void>();
      const poller = createMailboxPoller<Row, Inbox | null>({
        scope: () => state.current,
        readCursor: (inbox) => inbox?.cursor ?? 0,
        writeCursor: (c, inbox) => {
          if (inbox !== null) inbox.cursor = c;
        },
        fetch: async (after) => journal([8, 9])(after),
        dispatch: async (events, inbox) => {
          inDispatch.resolve();
          await finish.promise;
          for (const e of events) inbox?.requests.push(e.id);
        },
      });
      state.current = a;
      const run = poller.poll(0);
      await inDispatch.promise;
      // The account changes while A's dispatch is still running.
      state.current = b;
      finish.resolve();
      await expect(run).resolves.toBe("dispatched");
      expect(a.cursor).toBe(9);
      expect(a.requests).toEqual([8, 9]);
      expect(b.cursor).toBe(7);
      expect(b.requests).toEqual([]);
    });
  });
});
