// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { planReadmission, type DirectoryDevice } from "./device-readmission";

const kp = (tag: string): DirectoryDevice => ({
  deviceKeyB64: tag,
  keyPackage: new TextEncoder().encode(`kp:${tag}`),
});

describe("planReadmission (ADR-0050 section 2.4)", () => {
  it("admits a listed device that holds no leaf, for any account in the chat", () => {
    const plan = planReadmission({
      roster: [
        { leafKeyB64: "me-1", personKeyB64: "me" },
        { leafKeyB64: "bob-1", personKeyB64: "bob" },
      ],
      ownPersonKeyB64: "me",
      accounts: [
        { personKeyB64: "me", devices: [kp("me-1"), kp("me-2")] },
        { personKeyB64: "bob", devices: [kp("bob-1"), kp("bob-2")] },
      ],
      removed: new Set(),
      ownDeviceGroup: new Set(["me-1", "me-2"]),
    });
    expect(plan.admit.map((d) => d.deviceKeyB64)).toEqual(["me-2", "bob-2"]);
    expect(plan.retire).toEqual([]);
  });

  it("never re-admits a device this client saw removed from the chat", () => {
    const plan = planReadmission({
      roster: [{ leafKeyB64: "bob-1", personKeyB64: "bob" }],
      ownPersonKeyB64: "me",
      accounts: [{ personKeyB64: "bob", devices: [kp("bob-1"), kp("bob-old")] }],
      removed: new Set(["bob-old"]),
      ownDeviceGroup: new Set(["me-1", "me-2"]),
    });
    expect(plan.admit).toEqual([]);
  });

  it("retires an own leaf whose directory row is gone, and nobody else's", () => {
    const plan = planReadmission({
      roster: [
        { leafKeyB64: "me-1", personKeyB64: "me" },
        { leafKeyB64: "me-forgotten", personKeyB64: "me" },
        { leafKeyB64: "bob-gone", personKeyB64: "bob" },
      ],
      ownPersonKeyB64: "me",
      accounts: [
        { personKeyB64: "me", devices: [kp("me-1")] },
        { personKeyB64: "bob", devices: [] },
      ],
      removed: new Set(),
      ownDeviceGroup: new Set(["me-1", "me-2"]),
    });
    expect(plan.retire).toEqual(["me-forgotten"]);
    expect(plan.admit).toEqual([]);
  });

  it("decides nothing for an account whose directory fetch failed", () => {
    const plan = planReadmission({
      roster: [
        { leafKeyB64: "me-1", personKeyB64: "me" },
        { leafKeyB64: "me-2", personKeyB64: "me" },
      ],
      ownPersonKeyB64: "me",
      accounts: [{ personKeyB64: "me", devices: null }],
      removed: new Set(),
      ownDeviceGroup: new Set(["me-1", "me-2"]),
    });
    expect(plan).toEqual({ admit: [], retire: [] });
  });

  it("proposes each device once even when two accounts list it", () => {
    const plan = planReadmission({
      roster: [],
      ownPersonKeyB64: "me",
      accounts: [
        { personKeyB64: "a", devices: [kp("x")] },
        { personKeyB64: "b", devices: [kp("x")] },
      ],
      removed: new Set(),
      ownDeviceGroup: new Set(["me-1", "me-2"]),
    });
    expect(plan.admit).toHaveLength(1);
  });

  // ADR-0066 section 2.3 - the DeviceGroup filter.
  describe("own-account devices are admitted only from the DeviceGroup roster", () => {
    const base = {
      roster: [
        { leafKeyB64: "me-1", personKeyB64: "me" },
        { leafKeyB64: "bob-1", personKeyB64: "bob" },
      ],
      ownPersonKeyB64: "me",
      accounts: [
        { personKeyB64: "me", devices: [kp("me-1"), kp("me-2"), kp("me-removed")] },
        { personKeyB64: "bob", devices: [kp("bob-1"), kp("bob-2")] },
      ],
      removed: new Set<string>(),
    };

    it("does not re-admit an own device that published itself but is not in the DeviceGroup", () => {
      const plan = planReadmission({ ...base, ownDeviceGroup: new Set(["me-1", "me-2"]) });
      expect(plan.admit.map((d) => d.deviceKeyB64)).toEqual(["me-2", "bob-2"]);
    });

    it("admits no own device while the DeviceGroup roster is unknown", () => {
      const plan = planReadmission({ ...base, ownDeviceGroup: null });
      expect(plan.admit.map((d) => d.deviceKeyB64)).toEqual(["bob-2"]);
    });

    it("leaves a contact's devices to the directory and the removed memory", () => {
      const plan = planReadmission({
        ...base,
        ownDeviceGroup: new Set(),
        removed: new Set(["bob-2"]),
      });
      expect(plan.admit).toEqual([]);
    });

    it("keeps the per-chat removed memory as a second test for an own device", () => {
      const plan = planReadmission({
        ...base,
        ownDeviceGroup: new Set(["me-1", "me-2", "me-removed"]),
        removed: new Set(["me-removed"]),
      });
      expect(plan.admit.map((d) => d.deviceKeyB64)).toEqual(["me-2", "bob-2"]);
    });

    it("does not retire an own leaf merely because the DeviceGroup lacks it", () => {
      // A device restored from the phrase with no other device online founds
      // its own DeviceGroup: two devices that are each absent from the
      // other's roster must not evict each other.
      const plan = planReadmission({
        roster: [
          { leafKeyB64: "me-1", personKeyB64: "me" },
          { leafKeyB64: "me-restored", personKeyB64: "me" },
        ],
        ownPersonKeyB64: "me",
        accounts: [{ personKeyB64: "me", devices: [kp("me-1"), kp("me-restored")] }],
        removed: new Set(),
        ownDeviceGroup: new Set(["me-1"]),
      });
      expect(plan).toEqual({ admit: [], retire: [] });
    });
  });
});
