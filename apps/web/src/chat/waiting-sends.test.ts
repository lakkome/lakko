// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, describe, expect, it } from "vitest";
import { DsHttpError, DsNetworkError } from "../ds/client";
import {
  noteConnectionEvent,
  resetConnectionStateForTest,
} from "../net/connection-state";
import {
  MAX_AUTO_SENDS,
  WaitingSendPass,
  ackAfterAutoNetworkFailure,
  isNetworkSendFailure,
  mustQueueBehind,
  runWhenServersAnswer,
  type SendMode,
  type SendOutcome,
} from "./waiting-sends";

afterEach(() => resetConnectionStateForTest());

type Ack = "pending" | "sent" | "failed" | "waiting";

/**
 * A conversation as `routes/Chat.tsx` drives it: rows by temp id, a network
 * that is up or down, rows the server refuses. `send` is the route's
 * `publishOutgoing` reduced to what section 7 decides.
 */
class Conversation {
  readonly rows = new Map<number, { ack: Ack; autoSends: number }>();
  readonly published: number[] = [];
  readonly attempts: number[] = [];
  networkUp = false;
  readonly refused = new Set<number>();
  readonly pass = new WaitingSendPass({
    waitingInOrder: () =>
      [...this.rows.entries()]
        .filter(([, r]) => r.ack === "waiting")
        .map(([id]) => id)
        .sort((a, b) => a - b),
    send: (id) => this.send(id, "automatic"),
  });

  async send(id: number, mode: SendMode): Promise<SendOutcome> {
    const row = this.rows.get(id);
    if (row === undefined) return "sent";
    if (
      mode !== "automatic" &&
      mustQueueBehind(this.pass_waiting(), id, this.pass.isRunning)
    ) {
      row.ack = "waiting";
      row.autoSends = 0;
      return "network";
    }
    this.attempts.push(id);
    await Promise.resolve();
    if (this.refused.has(id)) {
      row.ack = "failed";
      return "refused";
    }
    if (!this.networkUp) {
      const spent = mode === "automatic" ? row.autoSends + 1 : 0;
      row.autoSends = spent;
      row.ack =
        mode === "automatic" ? ackAfterAutoNetworkFailure(spent) : "waiting";
      return "network";
    }
    row.ack = "sent";
    this.published.push(id);
    return "sent";
  }

  private pass_waiting(): number[] {
    return [...this.rows.entries()]
      .filter(([, r]) => r.ack === "waiting")
      .map(([id]) => id);
  }

  /** The person sends message `id` now. */
  async write(id: number): Promise<void> {
    this.rows.set(id, { ack: "pending", autoSends: 0 });
    await this.send(id, "first");
  }
}

/** Let the pass and its awaits settle. */
const settle = async (): Promise<void> => {
  for (let i = 0; i < 50; i += 1) await Promise.resolve();
};

describe("waiting sends (docs/ux/connection-state.md section 7)", () => {
  it("a network-failed message goes by itself once, in order, when the servers answer again", async () => {
    const c = new Conversation();
    const stop = runWhenServersAnswer(c.pass);
    await c.write(1);
    await c.write(2);
    expect(c.rows.get(1)?.ack).toBe("waiting");
    expect(c.rows.get(2)?.ack).toBe("waiting");
    expect(c.published).toEqual([]);
    noteConnectionEvent("network-failure");
    c.networkUp = true;
    noteConnectionEvent("answered");
    await settle();
    expect(c.published).toEqual([1, 2]);
    // Another drop and return sends nothing more: each went once.
    noteConnectionEvent("network-failure");
    noteConnectionEvent("answered");
    await settle();
    expect(c.published).toEqual([1, 2]);
    stop();
  });

  it("the system's signal alone does not send", async () => {
    const c = new Conversation();
    const stop = runWhenServersAnswer(c.pass);
    await c.write(1);
    noteConnectionEvent("os-offline");
    noteConnectionEvent("os-online");
    await settle();
    expect(c.attempts).toEqual([1]);
    stop();
  });

  it("a pass stops at a network failure without spending the later messages' attempts", async () => {
    const c = new Conversation();
    await c.write(1);
    await c.write(2);
    await c.write(3);
    await c.pass.run();
    expect(c.rows.get(1)).toEqual({ ack: "waiting", autoSends: 1 });
    expect(c.rows.get(2)).toEqual({ ack: "waiting", autoSends: 0 });
    expect(c.rows.get(3)).toEqual({ ack: "waiting", autoSends: 0 });
  });

  it("a refused message is not sent by itself", async () => {
    const c = new Conversation();
    const stop = runWhenServersAnswer(c.pass);
    c.networkUp = true;
    c.refused.add(1);
    await c.write(1);
    expect(c.rows.get(1)?.ack).toBe("failed");
    noteConnectionEvent("network-failure");
    noteConnectionEvent("answered");
    await settle();
    expect(c.attempts).toEqual([1]);
    stop();
  });

  it(`gives a message at most ${MAX_AUTO_SENDS} automatic sends, then Not sent`, async () => {
    const c = new Conversation();
    await c.write(1);
    for (let i = 1; i <= MAX_AUTO_SENDS; i += 1) {
      await c.pass.run();
      expect(c.rows.get(1)?.autoSends).toBe(i);
    }
    expect(c.rows.get(1)?.ack).toBe("failed");
    await c.pass.run();
    expect(c.attempts.length).toBe(1 + MAX_AUTO_SENDS);
  });

  it("a new message queues behind a waiting one and goes in turn", async () => {
    const c = new Conversation();
    await c.write(1);
    c.networkUp = true;
    await c.write(2);
    expect(c.rows.get(2)?.ack).toBe("waiting");
    expect(c.published).toEqual([]);
    await c.pass.run();
    expect(c.published).toEqual([1, 2]);
  });

  it("one pass at a time: two requests never send a row twice", async () => {
    const c = new Conversation();
    await c.write(1);
    await c.write(2);
    c.networkUp = true;
    await Promise.all([c.pass.run(), c.pass.run(), c.pass.run()]);
    expect(c.published).toEqual([1, 2]);
  });

  it("only a request with no answer is a network failure", () => {
    const wrap = (cause: unknown): Error =>
      Object.assign(new Error("chat"), { cause });
    expect(
      isNetworkSendFailure(
        wrap(new DsNetworkError("/ds/x", new TypeError("Failed to fetch"))),
        false,
      ),
    ).toBe(true);
    expect(
      isNetworkSendFailure(
        { variant: { cause: new DsNetworkError("/ds/x", "x") } },
        false,
      ),
    ).toBe(true);
    expect(isNetworkSendFailure(new TypeError("Failed to fetch"), false)).toBe(
      true,
    );
    expect(
      isNetworkSendFailure(wrap(new DsHttpError(403, "/ds/x", "")), false),
    ).toBe(false);
    expect(
      isNetworkSendFailure(new DOMException("aborted", "AbortError"), false),
    ).toBe(false);
    expect(isNetworkSendFailure(new Error("compose"), false)).toBe(false);
    // A suspended account's refusal is raised as a transport failure; it is not one.
    expect(
      isNetworkSendFailure(
        new DsNetworkError("/ds/x", "the account is suspended"),
        true,
      ),
    ).toBe(false);
  });
});
