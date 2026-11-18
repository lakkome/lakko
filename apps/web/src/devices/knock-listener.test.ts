// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 third amendment, point 1 - a foreground page keeps listening for
 * knocks after it admitted one, after a failed read and after a failed
 * answer; it stops only when its signal aborts, and says so in the log.
 * Seen live 2026-11-11: a page stopped answering after admitting, and a
 * restored device founded the DeviceGroup over it.
 */
import { describe, expect, it } from "vitest";

import { runKnockListener, type KnockListenerPorts, type KnockRow } from "./knock-listener";

function row(id: number): KnockRow {
  return { id, payload: new Uint8Array([id]), createdAt: "2026-11-11T20:00:00Z" };
}

type Step = readonly KnockRow[] | Error;

function harness(steps: Step[], options: { answerFails?: (rows: readonly KnockRow[]) => boolean } = {}) {
  const abort = new AbortController();
  const log: string[] = [];
  const answered: number[][] = [];
  let visible = true;
  let reads = 0;
  const ports: KnockListenerPorts = {
    visible: () => visible,
    async untilVisible() {
      visible = true;
    },
    async read() {
      reads += 1;
      const step = steps.shift();
      if (step === undefined) {
        // Nothing more scripted: the session ends.
        abort.abort();
        return [];
      }
      if (step instanceof Error) throw step;
      return step;
    },
    classify: (err) => ((err as Error).message === "offline" ? "network" : "other"),
    async answer(rows) {
      answered.push(rows.map((r) => r.id));
      if (options.answerFails?.(rows) === true) throw new Error("answer failed");
    },
    sleep: async () => undefined,
    now: () => 0,
    log: (line) => log.push(line),
  };
  return {
    abort,
    log,
    answered,
    reads: () => reads,
    hide: () => {
      visible = false;
    },
    run: () => runKnockListener(ports, abort.signal),
  };
}

describe("the knock listener", () => {
  it("keeps listening after it answered (and admitted) a knock", async () => {
    // The first knock is admitted; the second one, minutes later, is still
    // answered by the same loop.
    const h = harness([[row(1)], [], [row(2)]]);
    await h.run();
    expect(h.answered).toEqual([[1], [2]]);
    expect(h.log[0]).toBe("DeviceGroup knock listener started");
    expect(h.log[h.log.length - 1]).toBe("DeviceGroup knock listener stopped");
  });

  it("a failed read or a failed answer does not stop it", async () => {
    const h = harness([new Error("offline"), new Error("500"), [row(1)], [row(2)]], {
      answerFails: (rows) => rows[0]?.id === 1,
    });
    await h.run();
    expect(h.answered).toEqual([[1], [2]]);
    expect(h.log).toContain("DeviceGroup knock read failed");
    expect(h.log).toContain("DeviceGroup knock answer failed");
  });

  it("stops only when its signal aborts, and never rejects", async () => {
    const h = harness([[row(1)]]);
    h.abort.abort();
    await expect(h.run()).resolves.toBeUndefined();
    expect(h.answered).toEqual([]);
    expect(h.log).toEqual(["DeviceGroup knock listener started", "DeviceGroup knock listener stopped"]);
  });

  it("a hidden page reads nothing until it is back, then goes on", async () => {
    const h = harness([[row(3)]]);
    h.hide();
    await h.run();
    expect(h.log).toContain("DeviceGroup knock listener paused: page in the background");
    expect(h.log).toContain("DeviceGroup knock listener resumed");
    expect(h.answered).toEqual([[3]]);
  });
});
