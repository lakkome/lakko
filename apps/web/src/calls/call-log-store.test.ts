// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { CallLogEntry } from "./call-log";
import {
  CALL_LOG_MAX_PER_GROUP,
  CallLogStore,
  decodeCallLogRow,
  encodeCallLogRow,
  foldCallLogEntry,
  mergeCallLogEntries,
  type CallLogBackend,
  type CallLogRows,
} from "./call-log-store";

const GROUP = "Z3JvdXAtb25l";
const OTHER = "Z3JvdXAtdHdv";

function entry(over: Partial<CallLogEntry> = {}): CallLogEntry {
  return {
    id: "call-1",
    outcome: "outgoing",
    video: false,
    startedAtMs: 1_000,
    durationMs: 5_000,
    ...over,
  };
}

/** A backend whose stored row is inspectable, with a load the test can hold
 *  open so a record can land while it is in flight. */
class FakeBackend implements CallLogBackend {
  saved = new Map<string, CallLogRows>();
  loadResult: CallLogRows | null = null;
  private gate: Promise<void> | null = null;
  openGate: (() => void) | null = null;

  /** Make the next `load` wait until {@link openGate} is called. */
  hold(): void {
    this.gate = new Promise<void>((resolve) => {
      this.openGate = resolve;
    });
  }

  async load(): Promise<CallLogRows | null> {
    if (this.gate !== null) await this.gate;
    return this.loadResult;
  }

  async save(accountId: string, rows: CallLogRows): Promise<void> {
    this.saved.set(accountId, new Map(rows));
  }
}

describe("foldCallLogEntry", () => {
  it("keeps one row per call id, however often it is reported", () => {
    // A session can report its terminal state twice (a local end racing the
    // peer's End on the journal); a call is one event either way.
    const once = foldCallLogEntry([], entry());
    const twice = foldCallLogEntry(once, entry());
    expect(twice).toHaveLength(1);
    expect(twice).toBe(once);
  });

  it("lets a later report correct an earlier one", () => {
    const first = foldCallLogEntry([], entry({ outcome: "missed", durationMs: null }));
    const corrected = foldCallLogEntry(first, entry({ outcome: "incoming", durationMs: 9_000 }));
    expect(corrected).toHaveLength(1);
    expect(corrected[0]?.outcome).toBe("incoming");
  });

  it("orders rows oldest first, whatever order they arrive in", () => {
    let list = foldCallLogEntry([], entry({ id: "b", startedAtMs: 2_000 }));
    list = foldCallLogEntry(list, entry({ id: "a", startedAtMs: 1_000 }));
    list = foldCallLogEntry(list, entry({ id: "c", startedAtMs: 3_000 }));
    expect(list.map((e) => e.id)).toEqual(["a", "b", "c"]);
  });

  it("caps the list, keeping the newest", () => {
    let list: readonly CallLogEntry[] = [];
    for (let i = 0; i < CALL_LOG_MAX_PER_GROUP + 10; i += 1) {
      list = foldCallLogEntry(list, entry({ id: `c${i}`, startedAtMs: i }));
    }
    expect(list).toHaveLength(CALL_LOG_MAX_PER_GROUP);
    expect(list[0]?.id).toBe("c10");
    expect(list[list.length - 1]?.id).toBe(`c${CALL_LOG_MAX_PER_GROUP + 9}`);
  });
});

describe("mergeCallLogEntries", () => {
  it("unions two lists by call id", () => {
    const a = [entry({ id: "a", startedAtMs: 1 })];
    const b = [entry({ id: "b", startedAtMs: 2 }), entry({ id: "a", startedAtMs: 1 })];
    expect(mergeCallLogEntries(a, b).map((e) => e.id)).toEqual(["a", "b"]);
  });
});

describe("call-log row codec", () => {
  it("round-trips", () => {
    const rows: CallLogRows = new Map([
      [GROUP, [entry({ id: "a", startedAtMs: 1 }), entry({ id: "b", startedAtMs: 2 })]],
      [OTHER, [entry({ id: "c", outcome: "missed", durationMs: null })]],
    ]);
    const back = decodeCallLogRow(encodeCallLogRow(rows));
    expect(back).not.toBeNull();
    expect(back?.get(GROUP)?.map((e) => e.id)).toEqual(["a", "b"]);
    expect(back?.get(OTHER)?.[0]?.outcome).toBe("missed");
  });

  it("refuses a payload that is not this schema", () => {
    expect(decodeCallLogRow("not json")).toBeNull();
    expect(decodeCallLogRow(JSON.stringify({ v: 2, g: {} }))).toBeNull();
    expect(decodeCallLogRow(JSON.stringify(["nope"]))).toBeNull();
  });

  it("drops a malformed entry instead of rendering it as undefined later", () => {
    const json = JSON.stringify({
      v: 1,
      g: {
        [GROUP]: [
          entry({ id: "good" }),
          { id: "bad-outcome", outcome: "exploded", video: false, startedAtMs: 1, durationMs: null },
          { id: "answered-is-not-an-outcome", outcome: "answered", video: false, startedAtMs: 1, durationMs: 0 },
          { id: "no-stamp", outcome: "missed", video: false, durationMs: null },
          { id: "negative", outcome: "incoming", video: false, startedAtMs: 1, durationMs: -1 },
          { outcome: "missed", video: false, startedAtMs: 1, durationMs: null },
          "not an object",
          null,
        ],
      },
    });
    const back = decodeCallLogRow(json);
    expect(back?.get(GROUP)?.map((e) => e.id)).toEqual(["good"]);
  });

  it("keeps an empty account decodable", () => {
    expect(decodeCallLogRow(encodeCallLogRow(new Map()))?.size).toBe(0);
  });
});

describe("CallLogStore", () => {
  it("records, reads back and persists", async () => {
    const backend = new FakeBackend();
    const store = new CallLogStore(backend);
    await store.open("acct");
    expect(store.record("acct", GROUP, entry())).toBe(true);
    expect(store.of(GROUP).map((e) => e.id)).toEqual(["call-1"]);
    await store.flush();
    expect(backend.saved.get("acct")?.get(GROUP)).toHaveLength(1);
  });

  it("drops a call for an account it is not bound to", async () => {
    const store = new CallLogStore(new FakeBackend());
    await store.open("acct");
    expect(store.record("someone-else", GROUP, entry())).toBe(false);
    expect(store.of(GROUP)).toHaveLength(0);
  });

  it("merges a call recorded before the load settled over the loaded row", async () => {
    // The window is real: a call can end in the first second of a session, and
    // the load is in flight for exactly that long.
    const backend = new FakeBackend();
    backend.loadResult = new Map([[GROUP, [entry({ id: "stored", startedAtMs: 1 })]]]);
    backend.hold();
    const store = new CallLogStore(backend);
    const opening = store.open("acct");
    store.record("acct", GROUP, entry({ id: "live", startedAtMs: 2 }));
    backend.openGate?.();
    await opening;
    expect(store.of(GROUP).map((e) => e.id)).toEqual(["stored", "live"]);
  });

  it("does not write a partial row while a load is in flight", async () => {
    const backend = new FakeBackend();
    backend.loadResult = new Map([[GROUP, [entry({ id: "stored", startedAtMs: 1 })]]]);
    backend.hold();
    const store = new CallLogStore(backend);
    const opening = store.open("acct");
    store.record("acct", GROUP, entry({ id: "live", startedAtMs: 2 }));
    backend.openGate?.();
    await opening;
    await store.flush();
    // The stored row must still carry the row that was on disk.
    expect(backend.saved.get("acct")?.get(GROUP)?.map((e) => e.id)).toEqual(["stored", "live"]);
  });

  it("forgets a group's rows and leaves the others alone", async () => {
    const store = new CallLogStore(new FakeBackend());
    await store.open("acct");
    store.record("acct", GROUP, entry({ id: "a" }));
    store.record("acct", OTHER, entry({ id: "b" }));
    store.forgetGroup("acct", GROUP);
    expect(store.of(GROUP)).toHaveLength(0);
    expect(store.of(OTHER)).toHaveLength(1);
  });

  it("empties in memory on close without touching what is on disk", async () => {
    const backend = new FakeBackend();
    const store = new CallLogStore(backend);
    await store.open("acct");
    store.record("acct", GROUP, entry());
    await store.flush();
    store.close();
    expect(store.of(GROUP)).toHaveLength(0);
    expect(store.boundAccount()).toBeNull();
    expect(backend.saved.get("acct")?.get(GROUP)).toHaveLength(1);
  });

  it("notifies subscribers on a change and not on a no-op", async () => {
    const store = new CallLogStore(new FakeBackend());
    await store.open("acct");
    let seen = 0;
    const off = store.subscribe(() => {
      seen += 1;
    });
    store.record("acct", GROUP, entry());
    expect(seen).toBe(1);
    store.record("acct", GROUP, entry());
    expect(seen).toBe(1);
    off();
    store.record("acct", GROUP, entry({ id: "another" }));
    expect(seen).toBe(1);
  });

  it("surfaces a persist failure instead of throwing into the caller", async () => {
    const errors: unknown[] = [];
    const failing: CallLogBackend = {
      load: async () => null,
      save: async () => {
        throw new Error("quota");
      },
    };
    const store = new CallLogStore(failing, (err) => errors.push(err));
    await store.open("acct");
    store.record("acct", GROUP, entry());
    await store.flush();
    expect(errors).toHaveLength(1);
  });
});
