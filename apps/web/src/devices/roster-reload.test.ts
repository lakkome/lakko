// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** docs/ux/second-device.md section 3 - the list follows the roster. */
import { createRoot, createSignal } from "solid-js";
import { describe, expect, it, vi } from "vitest";

import { reloadWhenRosterChanges, rosterListKey } from "./roster-reload";
import type { DeviceRoster } from "./sync/DeviceGroupSyncContext";

const roster = (keys: readonly string[], ownKey = "a", addedHere: readonly string[] = []): DeviceRoster => ({
  keys,
  ownKey,
  addedHere,
});

function watch(first: DeviceRoster | null) {
  const reload = vi.fn();
  const [value, set] = createSignal<DeviceRoster | null>(first);
  const dispose = createRoot((d) => {
    reloadWhenRosterChanges(value, reload);
    return d;
  });
  return { reload, set, dispose };
}

describe("rosterListKey", () => {
  it("is the same for the same devices in any order, whoever let them in", () => {
    expect(rosterListKey(roster(["a", "b"]))).toBe(rosterListKey(roster(["b", "a"], "a", ["b"])));
  });
  it("differs when a device comes or goes, and when this device differs", () => {
    expect(rosterListKey(roster(["a", "b"]))).not.toBe(rosterListKey(roster(["a"])));
    expect(rosterListKey(roster(["a", "b"]))).not.toBe(rosterListKey(roster(["a", "b"], "b")));
  });
  it("has nothing to say with no roster", () => {
    expect(rosterListKey(null)).toBeNull();
  });
});

describe("reloadWhenRosterChanges", () => {
  it("does not reload for the roster the view opened with", () => {
    const { reload, dispose } = watch(roster(["a"]));
    expect(reload).not.toHaveBeenCalled();
    dispose();
  });

  it("reloads when a device is added and when one is removed", () => {
    const { reload, set, dispose } = watch(roster(["a"]));
    set(roster(["a", "b"]));
    expect(reload).toHaveBeenCalledTimes(1);
    set(roster(["a"]));
    expect(reload).toHaveBeenCalledTimes(2);
    dispose();
  });

  it("reloads when the first roster arrives after the view opened", () => {
    const { reload, set, dispose } = watch(null);
    set(roster(["a"]));
    expect(reload).toHaveBeenCalledTimes(1);
    dispose();
  });

  it("does not reload for a roster holding the same devices", () => {
    const { reload, set, dispose } = watch(roster(["a", "b"]));
    set(roster(["b", "a"]));
    expect(reload).not.toHaveBeenCalled();
    dispose();
  });

  it("does not reload when the roster goes away, nor after the view closed", () => {
    const { reload, set, dispose } = watch(roster(["a"]));
    set(null);
    expect(reload).not.toHaveBeenCalled();
    dispose();
    set(roster(["a", "b"]));
    expect(reload).not.toHaveBeenCalled();
  });
});
