// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/connection-state.md` section 3: every receive loop retries at once
 * when the network returns, so "Connecting..." lasts until the first answer
 * and not until the next backoff tick.
 */

import { afterEach, describe, expect, it } from "vitest";
import {
  noteConnectionEvent,
  resetConnectionStateForTest,
} from "./connection-state";
import { runLongPollLoop, sleepCancellable } from "../ds/long-poll-loop";
import { GroupBackoff } from "../chat/background-receive-scheduler";
import { pollWhileVisible } from "../channels/open-channel-poll";

afterEach(() => resetConnectionStateForTest());

const networkComesBack = (): void => {
  noteConnectionEvent("network-failure");
  noteConnectionEvent("os-online");
};

describe("receive loops and the network coming back", () => {
  it("a long-poll loop starts its backoff again from the first step", async () => {
    const controller = new AbortController();
    const backoffs: number[] = [];
    let ticks = 0;
    await runLongPollLoop(controller.signal, {
      shouldContinue: () => ticks < 4,
      isPaused: () => false,
      waitUntilResumed: async () => undefined,
      tick: async () => {
        ticks += 1;
        if (ticks === 3) networkComesBack();
        throw new TypeError("Failed to fetch");
      },
      classifyFailure: () => "network",
      sleep: async () => undefined,
      now: () => 0,
      onFailure: (_err, ms) => backoffs.push(ms),
    });
    expect(backoffs).toEqual([2_000, 4_000, 2_000, 4_000]);
  });

  it("a sleeping loop wakes at once", async () => {
    networkComesBack();
    noteConnectionEvent("network-failure");
    const started = Date.now();
    const sleeping = sleepCancellable(60_000, new AbortController().signal);
    noteConnectionEvent("os-online");
    await sleeping;
    expect(Date.now() - started).toBeLessThan(5_000);
  });

  it("the background sweep drops network backoffs and keeps server-error ones", () => {
    const backoff = new GroupBackoff();
    backoff.failed("a", 0, "network");
    backoff.failed("b", 0, "other");
    expect(backoff.isBackedOff("a", 1)).toBe(true);
    backoff.networkReturned();
    expect(backoff.isBackedOff("a", 1)).toBe(false);
    expect(backoff.isBackedOff("b", 1)).toBe(true);
  });

  it("an open channel loads at once rather than at its next tick", async () => {
    let loads = 0;
    const stop = pollWhileVisible(
      async () => {
        loads += 1;
      },
      () => true,
      3_600_000,
    );
    networkComesBack();
    await Promise.resolve();
    expect(loads).toBe(1);
    stop();
  });
});
