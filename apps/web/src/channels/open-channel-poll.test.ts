// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { pollWhileVisible, samePosts } from "./open-channel-poll";

describe("pollWhileVisible (D9)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("asks on the tick while visible, and stops when told", async () => {
    const load = vi.fn(async () => {});
    const stop = pollWhileVisible(load, () => true, 100);
    await vi.advanceTimersByTimeAsync(250);
    expect(load).toHaveBeenCalledTimes(2);
    stop();
    await vi.advanceTimersByTimeAsync(500);
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("asks nothing while hidden", async () => {
    const load = vi.fn(async () => {});
    const stop = pollWhileVisible(load, () => false, 100);
    await vi.advanceTimersByTimeAsync(500);
    expect(load).not.toHaveBeenCalled();
    stop();
  });

  it("never runs two at once, and a failure does not stop it", async () => {
    let release: (() => void) | undefined;
    const load = vi.fn(
      () =>
        new Promise<void>((_resolve, reject) => {
          release = () => reject(new Error("no"));
        }),
    );
    const stop = pollWhileVisible(load, () => true, 100);
    await vi.advanceTimersByTimeAsync(350);
    expect(load).toHaveBeenCalledTimes(1);
    release?.();
    await vi.advanceTimersByTimeAsync(100);
    expect(load).toHaveBeenCalledTimes(2);
    stop();
  });
});

describe("samePosts", () => {
  it("compares by cursor", () => {
    expect(samePosts([{ cursor: 1 }], [{ cursor: 1 }])).toBe(true);
    expect(samePosts([{ cursor: 1 }], [{ cursor: 1 }, { cursor: 2 }])).toBe(false);
    expect(samePosts([{ cursor: 1 }], [{ cursor: 2 }])).toBe(false);
  });
});
