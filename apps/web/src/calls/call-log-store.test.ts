// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { CallLogEntry, StandaloneCallEntry } from "./call-log";
import {
  CALL_LOG_MAX_PER_GROUP,
  CALL_LOG_MAX_STANDALONE,
  CallLogStore,
  decodeCallLogRow,
  encodeCallLogRow,
  foldCallLogEntry,
  foldStandaloneCallEntry,
  mergeCallLogEntries,
  type CallLogBackend,
  type CallLogData,
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

function standalone(over: Partial<StandaloneCallEntry> = {}): StandaloneCallEntry {
  return {
    id: "aabbccdd",
    outcome: "outgoing",
    video: true,
    startedAtMs: 1_000,
    durationMs: 60_000,
    ...over,
  };
}

function data(groups: CallLogRows, list: readonly StandaloneCallEntry[] = []): CallLogData {
  return { groups, standalone: list };
}

/** A backend whose stored row is inspectable, with a load the test can hold
 *  open so a record can land while it is in flight. */
class FakeBackend implements CallLogBackend {
  saved = new Map<string, CallLogData>();
  loadResult: CallLogData | null = null;
  private gate: Promise<void> | null = null;
  openGate: (() => void) | null = null;

  /** Make the next `load` wait until {@link openGate} is called. */
  hold(): void {
    this.gate = new Promise<void>((resolve) => {
      this.openGate = resolve;
    });
  }

  async load(): Promise<CallLogData | null> {
    if (this.gate !== null) await this.gate;
    return this.loadResult;
  }

  async save(accountId: string, stored: CallLogData): Promise<void> {
    this.saved.set(accountId, { groups: new Map(stored.groups), standalone: [...stored.standalone] });
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
    const back = decodeCallLogRow(encodeCallLogRow(data(rows)));
    expect(back).not.toBeNull();
    expect(back?.groups.get(GROUP)?.map((e) => e.id)).toEqual(["a", "b"]);
    expect(back?.groups.get(OTHER)?.[0]?.outcome).toBe("missed");
  });

  it("refuses a payload that is not this schema", () => {
    expect(decodeCallLogRow("not json")).toBeNull();
    // Version 1 (no calls without a conversation) is not read: there is no
    // stored data to carry over.
    expect(decodeCallLogRow(JSON.stringify({ v: 1, g: {} }))).toBeNull();
    expect(decodeCallLogRow(JSON.stringify({ v: 3, g: {}, n: [] }))).toBeNull();
    expect(decodeCallLogRow(JSON.stringify(["nope"]))).toBeNull();
  });

  it("drops a malformed entry instead of rendering it as undefined later", () => {
    const json = JSON.stringify({
      v: 2,
      n: [],
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
    expect(back?.groups.get(GROUP)?.map((e) => e.id)).toEqual(["good"]);
  });

  it("keeps an empty account decodable", () => {
    const back = decodeCallLogRow(encodeCallLogRow(data(new Map())));
    expect(back?.groups.size).toBe(0);
    expect(back?.standalone).toEqual([]);
  });

  it("round-trips the calls with no conversation", () => {
    const list = [
      standalone({ id: "a1", startedAtMs: 1 }),
      standalone({ id: "b2", startedAtMs: 2, outcome: "incoming", video: false }),
    ];
    const back = decodeCallLogRow(encodeCallLogRow(data(new Map(), list)));
    expect(back?.standalone).toEqual(list);
  });

  it("drops a call with no conversation it could not have written", () => {
    const json = JSON.stringify({
      v: 2,
      g: {},
      n: [
        standalone({ id: "good" }),
        // Such a call never rings, so it is never missed or declined.
        { ...standalone({ id: "missed" }), outcome: "missed" },
        { ...standalone({ id: "declined" }), outcome: "declined" },
        { ...standalone({ id: "no-stamp" }), startedAtMs: "soon" },
        "not an object",
      ],
    });
    expect(decodeCallLogRow(json)?.standalone.map((e) => e.id)).toEqual(["good"]);
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
    expect(backend.saved.get("acct")?.groups.get(GROUP)).toHaveLength(1);
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
    backend.loadResult = data(new Map([[GROUP, [entry({ id: "stored", startedAtMs: 1 })]]]));
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
    backend.loadResult = data(new Map([[GROUP, [entry({ id: "stored", startedAtMs: 1 })]]]));
    backend.hold();
    const store = new CallLogStore(backend);
    const opening = store.open("acct");
    store.record("acct", GROUP, entry({ id: "live", startedAtMs: 2 }));
    backend.openGate?.();
    await opening;
    await store.flush();
    // The stored row must still carry the row that was on disk.
    expect(backend.saved.get("acct")?.groups.get(GROUP)?.map((e) => e.id)).toEqual(["stored", "live"]);
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
    expect(backend.saved.get("acct")?.groups.get(GROUP)).toHaveLength(1);
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

describe("foldStandaloneCallEntry", () => {
  it("keeps one row per call id, however often it is reported", () => {
    const once = foldStandaloneCallEntry([], standalone());
    const twice = foldStandaloneCallEntry(once, standalone());
    expect(twice).toHaveLength(1);
    expect(twice).toBe(once);
  });

  it("keeps the first row when this device joins the same call again", () => {
    // A rejoin of the same call from a link is still one call, and its first
    // row stays as it is.
    const first = foldStandaloneCallEntry([], standalone({ startedAtMs: 1_000, durationMs: 10_000 }));
    const again = foldStandaloneCallEntry(
      first,
      standalone({ startedAtMs: 90_000, durationMs: 40_000, outcome: "incoming" }),
    );
    expect(again).toBe(first);
    expect(again).toHaveLength(1);
    expect(again[0]?.startedAtMs).toBe(1_000);
    expect(again[0]?.durationMs).toBe(10_000);
    expect(again[0]?.outcome).toBe("outgoing");
  });

  it("keeps different calls apart, oldest first", () => {
    let list = foldStandaloneCallEntry([], standalone({ id: "b", startedAtMs: 2 }));
    list = foldStandaloneCallEntry(list, standalone({ id: "a", startedAtMs: 1 }));
    expect(list.map((e) => e.id)).toEqual(["a", "b"]);
  });

  it("caps the list, keeping the newest", () => {
    let list: readonly StandaloneCallEntry[] = [];
    for (let i = 0; i < CALL_LOG_MAX_STANDALONE + 5; i += 1) {
      list = foldStandaloneCallEntry(list, standalone({ id: `c${i}`, startedAtMs: i }));
    }
    expect(list).toHaveLength(CALL_LOG_MAX_STANDALONE);
    expect(list[0]?.id).toBe("c5");
  });
});

describe("CallLogStore, calls with no conversation", () => {
  it("records one per call, persists it beside the conversations' rows", async () => {
    const backend = new FakeBackend();
    const store = new CallLogStore(backend);
    await store.open("acct");
    store.record("acct", GROUP, entry());
    expect(store.recordStandalone("acct", standalone())).toBe(true);
    expect(store.recordStandalone("acct", standalone())).toBe(false);
    // A rejoin of the same call writes nothing over the first row.
    expect(store.recordStandalone("acct", standalone({ durationMs: 1 }))).toBe(false);
    expect(store.standalone()[0]?.durationMs).toBe(60_000);
    expect(store.standalone().map((e) => e.id)).toEqual(["aabbccdd"]);
    await store.flush();
    const saved = backend.saved.get("acct");
    expect(saved?.groups.get(GROUP)).toHaveLength(1);
    expect(saved?.standalone.map((e) => e.id)).toEqual(["aabbccdd"]);
  });

  it("drops one for an account it is not bound to", async () => {
    const store = new CallLogStore(new FakeBackend());
    await store.open("acct");
    expect(store.recordStandalone("someone-else", standalone())).toBe(false);
    expect(store.standalone()).toHaveLength(0);
  });

  it("merges one recorded before the load settled over the loaded rows", async () => {
    const backend = new FakeBackend();
    backend.loadResult = data(new Map(), [standalone({ id: "stored", startedAtMs: 1 })]);
    backend.hold();
    const store = new CallLogStore(backend);
    const opening = store.open("acct");
    store.recordStandalone("acct", standalone({ id: "live", startedAtMs: 2 }));
    backend.openGate?.();
    await opening;
    expect(store.standalone().map((e) => e.id)).toEqual(["stored", "live"]);
  });

  it("keeps the stored row over one for the same call recorded during the load", async () => {
    const backend = new FakeBackend();
    backend.loadResult = data(new Map(), [standalone({ id: "same", durationMs: 5_000 })]);
    backend.hold();
    const store = new CallLogStore(backend);
    const opening = store.open("acct");
    store.recordStandalone("acct", standalone({ id: "same", durationMs: 9_000 }));
    backend.openGate?.();
    await opening;
    expect(store.standalone().map((e) => e.durationMs)).toEqual([5_000]);
  });

  it("empties in memory on close and on a switch to another account", async () => {
    const store = new CallLogStore(new FakeBackend());
    await store.open("acct");
    store.recordStandalone("acct", standalone());
    await store.open("other");
    expect(store.standalone()).toHaveLength(0);
    await store.open("acct");
    store.recordStandalone("acct", standalone());
    store.close();
    expect(store.standalone()).toHaveLength(0);
  });
});
