// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0024 — the durable read-watermark store: monotone folds, account
 * isolation, the row codec and the persistence round trip.
 */
import { describe, expect, it } from "vitest";

import {
  ReadWatermarkStore,
  decodeReadWatermarkRow,
  encodeReadWatermarkRow,
  mergeReadWatermarks,
  type ReadWatermarkBackend,
  type ReadWatermarkRows,
} from "./read-watermark-store";

const G1 = "Z3JvdXAtMQ==";
const G2 = "Z3JvdXAtMg==";
const BOB = "Ym9i";
const CAROL = "Y2Fyb2w=";

/** A backend holding encoded JSON per account, the shape the IDB row keeps. */
class FakeBackend implements ReadWatermarkBackend {
  readonly disk = new Map<string, string>();
  saves = 0;
  failLoad = false;
  pendingLoad: Promise<void> | null = null;

  async load(accountId: string): Promise<ReadWatermarkRows | null> {
    if (this.pendingLoad !== null) await this.pendingLoad;
    if (this.failLoad) throw new Error("load failed");
    const json = this.disk.get(accountId);
    return json === undefined ? null : decodeReadWatermarkRow(json);
  }

  async save(accountId: string, rows: ReadWatermarkRows): Promise<void> {
    this.saves += 1;
    this.disk.set(accountId, encodeReadWatermarkRow(rows));
  }
}

describe("ReadWatermarkStore — monotone", () => {
  it("advances and never lowers a sender's watermark", async () => {
    const store = new ReadWatermarkStore(new FakeBackend());
    await store.open("A");
    expect(store.ingest("A", G1, BOB, 10)).toBe(true);
    expect(store.ingest("A", G1, BOB, 7)).toBe(false);
    expect(store.ingest("A", G1, BOB, 10)).toBe(false);
    expect(store.of(G1).get(BOB)).toBe(10);
    expect(store.ingest("A", G1, BOB, 12)).toBe(true);
    expect(store.of(G1).get(BOB)).toBe(12);
  });

  it("keeps groups and senders apart", async () => {
    const store = new ReadWatermarkStore(new FakeBackend());
    await store.open("A");
    store.ingest("A", G1, BOB, 5);
    store.ingest("A", G1, CAROL, 3);
    store.ingest("A", G2, BOB, 9);
    expect([...store.of(G1)]).toEqual([
      [BOB, 5],
      [CAROL, 3],
    ]);
    expect(store.of(G2).get(BOB)).toBe(9);
    expect(store.of("unknown").size).toBe(0);
  });

  it("refuses a malformed receipt", async () => {
    const store = new ReadWatermarkStore(new FakeBackend());
    await store.open("A");
    expect(store.ingest("A", G1, "", 5)).toBe(false);
    expect(store.ingest("A", G1, BOB, -1)).toBe(false);
    expect(store.ingest("A", G1, BOB, 1.5)).toBe(false);
    expect(store.of(G1).size).toBe(0);
  });

  it("merges a loaded row with receipts that landed during the load, keeping the higher value", async () => {
    const backend = new FakeBackend();
    backend.disk.set(
      "A",
      encodeReadWatermarkRow(new Map([[G1, new Map([[BOB, 20], [CAROL, 1]])]])),
    );
    let release: () => void = () => undefined;
    backend.pendingLoad = new Promise<void>((r) => {
      release = r;
    });
    const store = new ReadWatermarkStore(backend);
    const opening = store.open("A");
    store.ingest("A", G1, BOB, 5);
    store.ingest("A", G1, CAROL, 8);
    // The save waits for the load: nothing hits disk while it is in flight.
    expect(backend.saves).toBe(0);
    release();
    await opening;
    await store.flush();
    expect(store.of(G1).get(BOB)).toBe(20);
    expect(store.of(G1).get(CAROL)).toBe(8);
    const onDisk = decodeReadWatermarkRow(backend.disk.get("A") ?? "");
    expect(onDisk?.get(G1)?.get(BOB)).toBe(20);
    expect(onDisk?.get(G1)?.get(CAROL)).toBe(8);
  });

  it("mergeReadWatermarks returns the input when nothing advanced", () => {
    const into = new Map([[BOB, 5]]);
    expect(mergeReadWatermarks(into, new Map([[BOB, 3]]))).toBe(into);
    expect(mergeReadWatermarks(into, new Map([[BOB, 6]])).get(BOB)).toBe(6);
  });
});

describe("ReadWatermarkStore — accounts", () => {
  it("drops a receipt for an account it is not bound to", async () => {
    const backend = new FakeBackend();
    const store = new ReadWatermarkStore(backend);
    expect(store.ingest("A", G1, BOB, 5)).toBe(false);
    await store.open("A");
    expect(store.ingest("B", G1, BOB, 5)).toBe(false);
    expect(store.of(G1).size).toBe(0);
  });

  it("switching accounts never shows or writes the other account's watermarks", async () => {
    const backend = new FakeBackend();
    const store = new ReadWatermarkStore(backend);
    await store.open("A");
    store.ingest("A", G1, BOB, 5);
    await store.flush();
    store.close();
    expect(store.of(G1).size).toBe(0);
    await store.open("B");
    expect(store.of(G1).size).toBe(0);
    store.ingest("B", G1, CAROL, 2);
    await store.flush();
    expect(decodeReadWatermarkRow(backend.disk.get("A") ?? "")?.get(G1)?.has(CAROL)).toBe(false);
    expect(decodeReadWatermarkRow(backend.disk.get("B") ?? "")?.get(G1)?.has(BOB)).toBe(false);
  });
});

describe("ReadWatermarkStore — persistence", () => {
  it("survives a reload (a fresh store over the same backend)", async () => {
    const backend = new FakeBackend();
    const first = new ReadWatermarkStore(backend);
    await first.open("A");
    first.ingest("A", G1, BOB, 42);
    first.ingest("A", G2, CAROL, 7);
    await first.flush();

    const second = new ReadWatermarkStore(backend);
    await second.open("A");
    expect(second.of(G1).get(BOB)).toBe(42);
    expect(second.of(G2).get(CAROL)).toBe(7);
  });

  it("coalesces a burst of receipts into few writes", async () => {
    const backend = new FakeBackend();
    const store = new ReadWatermarkStore(backend);
    await store.open("A");
    for (let i = 1; i <= 20; i += 1) store.ingest("A", G1, BOB, i);
    await store.flush();
    expect(backend.saves).toBe(1);
    expect(decodeReadWatermarkRow(backend.disk.get("A") ?? "")?.get(G1)?.get(BOB)).toBe(20);
  });

  it("an unreadable row starts empty and reports the error", async () => {
    const backend = new FakeBackend();
    backend.failLoad = true;
    const errors: unknown[] = [];
    const store = new ReadWatermarkStore(backend, (e) => errors.push(e));
    await store.open("A");
    expect(errors.length).toBe(1);
    expect(store.ingest("A", G1, BOB, 1)).toBe(true);
  });
});

describe("row codec", () => {
  it("round-trips", () => {
    const rows = new Map([
      [G1, new Map([[BOB, 3], [CAROL, 9]])],
      [G2, new Map([[BOB, 0]])],
    ]);
    const decoded = decodeReadWatermarkRow(encodeReadWatermarkRow(rows));
    expect(decoded).toEqual(rows);
  });

  it("rejects a foreign schema and drops malformed entries", () => {
    expect(decodeReadWatermarkRow("not json")).toBeNull();
    expect(decodeReadWatermarkRow(JSON.stringify({ v: 2, g: {} }))).toBeNull();
    const decoded = decodeReadWatermarkRow(
      JSON.stringify({
        v: 1,
        g: {
          [G1]: { [BOB]: 4, [CAROL]: "5", "": 1, x: -2, y: 1.5 },
          [G2]: "nope",
          empty: {},
        },
      }),
    );
    expect(decoded).toEqual(new Map([[G1, new Map([[BOB, 4]])]]));
  });
});
