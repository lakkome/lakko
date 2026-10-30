// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** What an accepted request still owes: the Welcome, and only then the ack. */
import { describe, expect, it, vi } from "vitest";

import { settleAnswer, type SettleAnswerPorts } from "./owed-answer";

function ports(over: Partial<SettleAnswerPorts> = {}) {
  const order: string[] = [];
  const step =
    (name: string) =>
    async (): Promise<void> => {
      order.push(name);
    };
  return {
    order,
    ports: {
      deliver: step("deliver"),
      noteLanded: step("noteLanded"),
      ack: step("ack"),
      ackRefused: () => false,
      done: step("done"),
      ...over,
    } satisfies SettleAnswerPorts,
  };
}

const fail = (): Promise<never> => Promise.reject(new Error("offline"));

describe("settleAnswer", () => {
  it("writes the Welcome, then the ack, then closes the entry", async () => {
    const p = ports();
    expect(await settleAnswer(false, p.ports)).toBe("settled");
    expect(p.order).toEqual(["deliver", "noteLanded", "ack", "done"]);
  });

  it("sends no ack while the Welcome has not landed, and keeps the entry", async () => {
    const p = ports({ deliver: fail });
    expect(await settleAnswer(false, p.ports)).toBe("owed");
    expect(p.order).toEqual([]);
  });

  it("a retry with the Welcome landed sends the ack alone", async () => {
    const p = ports();
    expect(await settleAnswer(true, p.ports)).toBe("settled");
    expect(p.order).toEqual(["ack", "done"]);
  });

  it("an ack lost to the network stays owed", async () => {
    const p = ports({ ack: fail });
    expect(await settleAnswer(false, p.ports)).toBe("owed");
    expect(p.order).toEqual(["deliver", "noteLanded"]);
  });

  it("an ack nothing can write ends it: the requester has the Welcome", async () => {
    const p = ports({ ack: fail, ackRefused: () => true });
    expect(await settleAnswer(false, p.ports)).toBe("settled");
    expect(p.order).toEqual(["deliver", "noteLanded", "done"]);
  });

  it("a mark that could not be written does not stop the ack", async () => {
    const ack = vi.fn(async () => undefined);
    const p = ports({ noteLanded: fail, ack });
    expect(await settleAnswer(false, p.ports)).toBe("settled");
    expect(ack).toHaveBeenCalledTimes(1);
  });

  it("logs a fixed reason", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    await settleAnswer(false, ports({ deliver: () => Promise.reject(new Error("mailbox abcdef")) }).ports);
    expect(warn.mock.calls.flat().join(" ")).not.toContain("abcdef");
    warn.mockRestore();
  });
});
