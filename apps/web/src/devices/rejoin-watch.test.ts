// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 third amendment - the rejoining device's long poll on its
 * knock's rendezvous: a presence note and a Welcome are told apart, other
 * rows are skipped, and silence only counts when every read succeeded (a
 * network failure must never make the device found the DeviceGroup).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { openRejoinWatch, type RejoinWatchPorts, type RendezvousPage } from "./rejoin-watch";

const PRESENT = new Uint8Array([0xaa]);
const OTHER = new Uint8Array([0xbb]);
const WELCOME = new Uint8Array([0x01, 0x02]);

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const t = setTimeout(resolve, ms);
    signal.addEventListener("abort", () => {
      clearTimeout(t);
      resolve();
    });
  });
}

/** Pages served in order; once they run out, a read waits until aborted. */
function ports(pages: (RendezvousPage | Error | "404")[]): RejoinWatchPorts & { reads: number[] } {
  const p = {
    reads: [] as number[],
    async fetchPage(after: number, _waitSecs: number, signal: AbortSignal): Promise<RendezvousPage> {
      p.reads.push(after);
      const next = pages.shift();
      if (next === undefined) {
        await sleep(60_000, signal);
        return { messages: [] };
      }
      if (next === "404") throw Object.assign(new Error("not found"), { status: 404 });
      if (next instanceof Error) throw next;
      return next;
    },
    isEmptyRendezvous: (err: unknown) => (err as { status?: number }).status === 404,
    isPresent: (row: Uint8Array) => row[0] === 0xaa,
    isOtherRow: (row: Uint8Array) => row[0] === 0xbb,
    now: () => Date.now(),
    sleep,
  };
  return p;
}

describe("the rejoin rendezvous watch", () => {
  it("returns a presence note, skips other rows, then returns the Welcome", async () => {
    const p = ports([
      {
        messages: [
          { id: 1, payload: PRESENT },
          { id: 2, payload: OTHER },
        ],
      },
      { messages: [{ id: 3, payload: WELCOME }] },
    ]);
    const parent = new AbortController();
    const watch = openRejoinWatch(p, parent.signal);
    expect(await watch.next(Date.now() + 2_000)).toEqual({ kind: "present" });
    expect(await watch.next(Date.now() + 2_000)).toEqual({ kind: "welcome", bytes: WELCOME });
    // The second read continued after the first page.
    expect(p.reads.slice(0, 2)).toEqual([0, 2]);
    watch.close();
  });

  it("silence while reading is null at the deadline", async () => {
    const watch = openRejoinWatch(ports([]), new AbortController().signal);
    const started = Date.now();
    expect(await watch.next(started + 50)).toBeNull();
    expect(Date.now() - started).toBeGreaterThanOrEqual(45);
    watch.close();
  });

  it("a fresh rendezvous answering 404 is still silence, not a failure", async () => {
    const watch = openRejoinWatch(ports(["404"]), new AbortController().signal);
    expect(await watch.next(Date.now() + 400)).toBeNull();
    watch.close();
  });

  it("a wait that ends with no answer after a failed read rejects instead of reading as silence", async () => {
    const watch = openRejoinWatch(ports([new Error("offline")]), new AbortController().signal);
    await expect(watch.next(Date.now() + 100)).rejects.toThrow("offline");
    watch.close();
  });

  it("an answer that arrives after a failed read still counts", async () => {
    const watch = openRejoinWatch(
      ports([new Error("blip"), { messages: [{ id: 1, payload: PRESENT }] }]),
      new AbortController().signal,
    );
    expect(await watch.next(Date.now() + 3_000)).toEqual({ kind: "present" });
    watch.close();
  });
});

describe("the presence note from the core", () => {
  beforeAll(async () => {
    const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
    await init({ module_or_path: readFileSync(wasmPath) });
  });

  it("is recognised exactly, and a Welcome-shaped row is not one", () => {
    const note = wasm.composeRejoinPresent();
    expect(note.length).toBe(25);
    expect(wasm.isRejoinPresent(note)).toBe(true);
    const changed = note.slice();
    changed[24] = 0x02;
    expect(wasm.isRejoinPresent(changed)).toBe(false);
    expect(wasm.isRejoinPresent(WELCOME)).toBe(false);
  });
});
