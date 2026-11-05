// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The update banner's Reload always reloads (base walk 2026-11-04:
 * "Reloading..." stood for over 10 s). Fakes stand in for the worker, the
 * container and the timer.
 */
import { describe, expect, it } from "vitest";

import { reloadIntoUpdate, UPDATE_RELOAD_FALLBACK_MS, type UpdateReloadReason } from "./update-reload";

function fakes(state: string, controlled = false) {
  const workerListeners: Array<() => void> = [];
  const containerListeners: Array<() => void> = [];
  const timers: Array<{ fn: () => void; ms: number }> = [];
  const posted: unknown[] = [];
  const reasons: UpdateReloadReason[] = [];
  let reloads = 0;
  const worker = {
    state,
    postMessage: (m: unknown) => posted.push(m),
    addEventListener: (_t: "statechange", fn: () => void) => workerListeners.push(fn),
  };
  const container = {
    controller: controlled ? worker : {},
    addEventListener: (_t: "controllerchange", fn: () => void) => containerListeners.push(fn),
  };
  const press = (): void =>
    reloadIntoUpdate(
      { worker, container, reload: () => (reloads += 1), setTimer: (fn, ms) => timers.push({ fn, ms }) },
      (why) => reasons.push(why),
    );
  return {
    worker,
    press,
    posted,
    reasons,
    reloads: () => reloads,
    fireController: () => containerListeners.forEach((f) => f()),
    setState: (s: string) => {
      worker.state = s;
      workerListeners.forEach((f) => f());
    },
    fireTimers: () => timers.forEach((t) => t.fn()),
    timers,
  };
}

describe("Reload into the waiting update", () => {
  it("the handshake: SKIP_WAITING, then a reload when the worker takes control", () => {
    const f = fakes("installed");
    f.press();
    expect(f.posted).toEqual([{ type: "SKIP_WAITING" }]);
    expect(f.reloads()).toBe(0);
    f.fireController();
    expect(f.reloads()).toBe(1);
    expect(f.reasons).toEqual(["took-control"]);
  });

  it("a worker already in control (another tab reloaded first) reloads at once", () => {
    const f = fakes("activated", true);
    f.press();
    expect(f.reloads()).toBe(1);
    expect(f.reasons).toEqual(["already-active"]);
    expect(f.posted).toEqual([]);
  });

  it("a worker past waiting but not in control reloads at once", () => {
    const f = fakes("activating");
    f.press();
    expect(f.reasons).toEqual(["already-active"]);
  });

  it("a redundant worker (replaced by a newer one) reloads at once", () => {
    const f = fakes("redundant");
    f.press();
    expect(f.reasons).toEqual(["redundant"]);
  });

  it("activation without a controllerchange still reloads", () => {
    const f = fakes("installed");
    f.press();
    f.setState("activated");
    expect(f.reasons).toEqual(["activated"]);
  });

  it("a swap that never signals reloads after the fallback, once", () => {
    const f = fakes("installed");
    f.press();
    expect(f.timers.map((t) => t.ms)).toEqual([UPDATE_RELOAD_FALLBACK_MS]);
    f.fireTimers();
    f.fireController();
    f.setState("activated");
    expect(f.reloads()).toBe(1);
    expect(f.reasons).toEqual(["timeout"]);
  });
});
