// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0098 step 3: suspended mode asks every ten minutes and on foreground. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SUSPENDED_REFRESH_INTERVAL_MS } from "./suspension";
import { startSuspensionChecks } from "./suspension-timer";

describe("startSuspensionChecks", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function start(): { refresh: ReturnType<typeof vi.fn>; foreground: () => void; stop: () => void } {
    const refresh = vi.fn(async () => "suspended");
    let listener: (() => void) | null = null;
    const stop = startSuspensionChecks({
      refresh,
      setInterval: (fn, ms) => setInterval(fn, ms),
      clearInterval: (h) => clearInterval(h as ReturnType<typeof setInterval>),
      onForeground: (fn) => {
        listener = fn;
        return () => {
          listener = null;
        };
      },
    });
    return { refresh, foreground: () => listener?.(), stop };
  }

  it("refreshes every ten minutes, not before", () => {
    expect(SUSPENDED_REFRESH_INTERVAL_MS).toBe(600_000);
    const { refresh } = start();
    vi.advanceTimersByTime(SUSPENDED_REFRESH_INTERVAL_MS - 1);
    expect(refresh).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(refresh).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(SUSPENDED_REFRESH_INTERVAL_MS);
    expect(refresh).toHaveBeenCalledTimes(2);
  });

  it("refreshes when the app comes back to the foreground", () => {
    const { refresh, foreground } = start();
    foreground();
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("stops both when suspended mode ends", () => {
    const { refresh, foreground, stop } = start();
    stop();
    foreground();
    vi.advanceTimersByTime(SUSPENDED_REFRESH_INTERVAL_MS * 3);
    expect(refresh).not.toHaveBeenCalled();
  });

  it("survives a refresh that fails", async () => {
    const refresh = vi.fn(async () => {
      throw new Error("offline");
    });
    const stop = startSuspensionChecks({
      refresh,
      setInterval: (fn, ms) => setInterval(fn, ms),
      clearInterval: (h) => clearInterval(h as ReturnType<typeof setInterval>),
      onForeground: () => () => undefined,
    });
    vi.advanceTimersByTime(SUSPENDED_REFRESH_INTERVAL_MS * 2);
    await Promise.resolve();
    expect(refresh).toHaveBeenCalledTimes(2);
    stop();
  });
});
