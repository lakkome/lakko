// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  planReadmission,
  readmissionRunsHere,
  type DirectoryDevice,
  type ReadmissionCaller,
} from "./device-readmission";

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

  it("does not admit an own device whose KeyPackage names another own key (ADR-0074)", () => {
    const plan = planReadmission({
      roster: [{ leafKeyB64: "me-1", personKeyB64: "me" }],
      ownPersonKeyB64: "me",
      accounts: [
        {
          personKeyB64: "me",
          devices: [kp("me-1"), { ...kp("me-2"), admissible: false }, { ...kp("me-3"), admissible: true }],
        },
      ],
      removed: new Set(),
      ownDeviceGroup: new Set(["me-1", "me-2", "me-3"]),
    });
    expect(plan.admit.map((d) => d.deviceKeyB64)).toEqual(["me-3"]);
    expect(plan.retire).toEqual([]);
  });

  it("counts a stale own row as listed: its existing leaf is not retired (ADR-0074)", () => {
    const plan = planReadmission({
      roster: [
        { leafKeyB64: "me-1", personKeyB64: "me" },
        // In this chat already, under the same person key.
        { leafKeyB64: "me-2", personKeyB64: "me" },
      ],
      ownPersonKeyB64: "me",
      accounts: [{ personKeyB64: "me", devices: [kp("me-1"), { ...kp("me-2"), admissible: false }] }],
      removed: new Set(),
      ownDeviceGroup: new Set(["me-1", "me-2"]),
    });
    expect(plan.admit).toEqual([]);
    expect(plan.retire).toEqual([]);
  });

  it("never applies the flag to a contact's device (ADR-0074)", () => {
    const plan = planReadmission({
      roster: [
        { leafKeyB64: "me-1", personKeyB64: "me" },
        { leafKeyB64: "bob-1", personKeyB64: "bob" },
      ],
      ownPersonKeyB64: "me",
      accounts: [{ personKeyB64: "bob", devices: [kp("bob-1"), { ...kp("bob-2"), admissible: false }] }],
      removed: new Set(),
      ownDeviceGroup: new Set(["me-1"]),
    });
    expect(plan.admit.map((d) => d.deviceKeyB64)).toEqual(["bob-2"]);
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

  // ADR-0074 section 2.3 - the DeviceGroup filter.
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

// ---- ADR-0073 R.2: the wider lookup ----------------------------------------

import { lookupStanding, readmissionCandidates } from "./device-readmission";

describe("readmissionCandidates (ADR-0073 R.2)", () => {
  const HEX_A = "0123456789abcdef0123456789abcdef";
  const HEX_B = "fedcba9876543210fedcba9876543210";
  it("looks up every roster person with a known account id, contact or not", () => {
    expect(
      readmissionCandidates({
        rosterPersonKeysB64: ["me", "anna-old", "bob", "stranger", "anna-old"],
        ownPersonKeyB64: "me",
        contacts: [
          { signaturePubkeyB64: "anna-new", previousSignaturePubkeysB64: ["anna-old"], accountIdHex: HEX_A },
        ],
        profileAccountIdHexOf: (k) => (k === "bob" ? HEX_B.toUpperCase() : null),
      }),
    ).toEqual([
      { rosterKeyB64: "anna-old", accountIdHex: HEX_A },
      { rosterKeyB64: "bob", accountIdHex: HEX_B },
    ]);
  });
  it("the contact row wins over the profile's claim, and junk ids are skipped", () => {
    expect(
      readmissionCandidates({
        rosterPersonKeysB64: ["anna", "bob"],
        ownPersonKeyB64: "me",
        contacts: [{ signaturePubkeyB64: "anna", accountIdHex: HEX_A }],
        profileAccountIdHexOf: (k) => (k === "anna" ? HEX_B : "not-hex"),
      }),
    ).toEqual([{ rosterKeyB64: "anna", accountIdHex: HEX_A }]);
  });
});

describe("lookupStanding (ADR-0073 R.2 / R.7 case 2)", () => {
  it("the pin or a proven predecessor of it is the same person", () => {
    expect(lookupStanding({ rosterKeyB64: "k", pin: { pinnedSignaturePubkeyB64: "k" } })).toBe("same-person");
    expect(
      lookupStanding({
        rosterKeyB64: "k0",
        pin: { pinnedSignaturePubkeyB64: "k1", provenPredecessorsB64: ["k0"] },
      }),
    ).toBe("same-person");
  });
  it("a key an accepted reset retired names the reset identity", () => {
    expect(
      lookupStanding({
        rosterKeyB64: "old",
        pin: { pinnedSignaturePubkeyB64: "new", supersededKeysB64: ["old"] },
      }),
    ).toBe("reset-identity");
  });
  it("an unrelated key, or no pin, attaches nobody's devices", () => {
    expect(lookupStanding({ rosterKeyB64: "mallory", pin: { pinnedSignaturePubkeyB64: "victim" } })).toBe("skip");
    expect(lookupStanding({ rosterKeyB64: "k", pin: undefined })).toBe("skip");
  });
  it("with no pin, only this device's verified binding of exactly that key is followed", () => {
    expect(lookupStanding({ rosterKeyB64: "k", pin: undefined, binding: { keyB64: "k" } })).toBe("same-person");
    // A member claiming the id of an account verified under another key.
    expect(lookupStanding({ rosterKeyB64: "mallory", pin: undefined, binding: { keyB64: "victim" } })).toBe("skip");
    // A pin always decides; a leftover binding cannot widen it.
    expect(
      lookupStanding({
        rosterKeyB64: "mallory",
        pin: { pinnedSignaturePubkeyB64: "victim" },
        binding: { keyB64: "mallory" },
      }),
    ).toBe("skip");
  });
});

describe("readmissionRunsHere (ADR-0050 section 2.4, the open chat)", () => {
  it("leaves an open chat to its view and every other chat to the sweep", () => {
    expect(readmissionRunsHere("view", true)).toBe(true);
    expect(readmissionRunsHere("background", true)).toBe(false);
    expect(readmissionRunsHere("background", false)).toBe(true);
    expect(readmissionRunsHere("view", false)).toBe(false);
  });

  it("gives every chat exactly one caller, open or not", () => {
    const callers: ReadmissionCaller[] = ["background", "view"];
    for (const chatOpen of [true, false]) {
      expect(callers.filter((c) => readmissionRunsHere(c, chatOpen))).toHaveLength(1);
    }
  });

  it("the open one-to-one of a contact whose only phone was restored admits the new phone", () => {
    // The contact's old leaf stays in the roster (its device forgot itself and
    // its directory row is gone); the restored phone lists a KeyPackage and
    // holds no leaf. The view of the open chat is the caller that runs.
    expect(readmissionRunsHere("view", true)).toBe(true);
    const restored = kp("w20-new");
    const plan = planReadmission({
      roster: [
        { leafKeyB64: "w17-1", personKeyB64: "w17" },
        { leafKeyB64: "w20-old", personKeyB64: "w20" },
      ],
      ownPersonKeyB64: "w17",
      accounts: [
        { personKeyB64: "w17", devices: [kp("w17-1")] },
        { personKeyB64: "w20", devices: [restored] },
      ],
      removed: new Set(),
      ownDeviceGroup: new Set(["w17-1"]),
    });
    expect(plan.admit).toEqual([restored]);
    // Another account's stale leaf is its own devices' to retire, never ours.
    expect(plan.retire).toEqual([]);
  });
});
