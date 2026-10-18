// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 decision 4 - a device that lost or fell behind its DeviceGroup
 * rejoins through a sibling; re-founding is a single-device account's.
 *
 *   - "behind" is a drained read still behind the DS, with no own commit in
 *     flight;
 *   - a sibling admits a lost device (listed, bound, knocked after the last
 *     time it saw that key added);
 *   - a removed device (deleted and tombstoned in the directory, so not
 *     listed) is not admitted;
 *   - the device's own run knocks, waits, joins;
 *   - a single-device account founds (`lifecycle.ts` with `mayFound`).
 */
import { describe, expect, it } from "vitest";

import {
  FALLBACK_ADMIT_AFTER_MS,
  REKNOCK_AFTER_MS,
  admissionVerdict,
  decodeAdmissionRecord,
  decodeRejoinState,
  encodeAdmissionRecord,
  encodeRejoinState,
  isBehind,
  newAdmissionRecord,
  noteAdded,
  noteHandled,
  rejoinOnce,
  shouldKnockAgain,
  type AdmissionInput,
  type RejoinPorts,
  type RejoinState,
} from "./device-group-rejoin";

describe("behind", () => {
  it("a drained read still behind the DS, with no own commit in flight", () => {
    expect(isBehind(5n, true, 7n, false)).toBe(true);
    expect(isBehind(5n, true, 5n, false)).toBe(false);
    // Rows still to read: they may bring the device forward.
    expect(isBehind(5n, false, 7n, false)).toBe(false);
    // The DS named no epoch (an unclaimed journal).
    expect(isBehind(5n, true, undefined, false)).toBe(false);
    // Its own commit in flight: the DS may hold its own next epoch.
    expect(isBehind(5n, true, 6n, true)).toBe(false);
  });
});

const SIBLING = "aa";
const LOST = "bb";
const OTHER = "cc";

function input(overrides: Partial<AdmissionInput> = {}): AdmissionInput {
  return {
    knockId: 10,
    knockAtMs: 1_000_000,
    keyHex: LOST,
    ownKeyHex: SIBLING,
    roster: [
      { leaf: 0, keyHex: SIBLING },
      { leaf: 1, keyHex: LOST },
      { leaf: 2, keyHex: OTHER },
    ],
    handled: new Set(),
    watchingSinceMs: 500_000,
    lastAddedAtMs: undefined,
    listed: true,
    policyRequiresAttestation: false,
    nowMs: 1_000_100,
    ...overrides,
  };
}

describe("a sibling's admission rule", () => {
  it("admits a lost device whose previous leaf is still in the roster", () => {
    expect(admissionVerdict(input())).toBe("admit");
  });

  it("admits a device whose leaf is gone too", () => {
    expect(
      admissionVerdict(input({ roster: [{ leaf: 0, keyHex: SIBLING }, { leaf: 2, keyHex: OTHER }] })),
    ).toBe("admit");
  });

  it("never admits a removed device: deleted from the directory and tombstoned there", () => {
    expect(admissionVerdict(input({ listed: false }))).toBe("not-listed");
  });

  it("does not admit twice: a knock older than the key's last addition is answered", () => {
    expect(admissionVerdict(input({ lastAddedAtMs: 1_000_050 }))).toBe("already-admitted");
    expect(admissionVerdict(input({ handled: new Set([10]) }))).toBe("handled");
    // A knock after the last addition (the device lost its state again) is new.
    expect(admissionVerdict(input({ lastAddedAtMs: 900_000 }))).toBe("admit");
  });

  it("ignores a knock from before it started watching (it cannot know whether it was answered)", () => {
    expect(admissionVerdict(input({ watchingSinceMs: 1_000_001 }))).toBe("before-watching");
  });

  it("the lowest other leaf admits at once; any other sibling after the fallback delay", () => {
    const notLowest = input({ ownKeyHex: OTHER });
    expect(admissionVerdict(notLowest)).toBe("not-mine-yet");
    expect(
      admissionVerdict({ ...notLowest, nowMs: notLowest.knockAtMs + FALLBACK_ADMIT_AFTER_MS }),
    ).toBe("admit");
  });

  it("the knocking key itself is never the designated admitter", () => {
    // LOST holds leaf 0 here; the lowest OTHER leaf (SIBLING, leaf 1) admits.
    const roster = [
      { leaf: 0, keyHex: LOST },
      { leaf: 1, keyHex: SIBLING },
    ];
    expect(admissionVerdict(input({ roster }))).toBe("admit");
    expect(admissionVerdict(input({ roster, ownKeyHex: LOST }))).toBe("own-key");
  });

  it("under the attested-removal policy a present previous leaf waits for the person", () => {
    expect(admissionVerdict(input({ policyRequiresAttestation: true }))).toBe("needs-attestation");
    expect(
      admissionVerdict(
        input({ policyRequiresAttestation: true, roster: [{ leaf: 0, keyHex: SIBLING }] }),
      ),
    ).toBe("admit");
  });
});

describe("the sibling's record", () => {
  it("keeps the latest addition per key and a bounded handled set", () => {
    let r = newAdmissionRecord(5);
    r = noteAdded(r, LOST, 100);
    r = noteAdded(r, LOST, 50);
    expect(r.lastAddedAtMs[LOST]).toBe(100);
    for (let i = 0; i < 300; i += 1) r = noteHandled(r, i);
    expect(r.handled).toHaveLength(256);
    expect(r.handled[0]).toBe(44);
    expect(decodeAdmissionRecord(encodeAdmissionRecord(r))).toEqual(r);
    expect(decodeAdmissionRecord(new TextEncoder().encode('{"v":2}'))).toBeNull();
  });
});

describe("the rejoining device's run", () => {
  function ports(welcomeAfter: number | null): RejoinPorts & { log: string[]; state: RejoinState | null } {
    let now = 1_000;
    let attempts = 0;
    const p = {
      log: [] as string[],
      state: null as RejoinState | null,
      async dropLocalState() {
        p.log.push("drop");
      },
      async publishDirectoryKeyPackage() {
        p.log.push("directory-kp");
      },
      async loadState() {
        return p.state;
      },
      async saveState(s: RejoinState) {
        p.state = s;
      },
      async knock(at: number) {
        p.log.push("knock");
        return { v: 1 as const, rendezvousB64: `r${at}`, joinerSnapshotB64: "s", knockedAtMs: at };
      },
      async awaitWelcome(_s: RejoinState, deadline: number) {
        attempts += 1;
        if (welcomeAfter !== null && attempts > welcomeAfter) return new Uint8Array([1]);
        now = deadline;
        return null;
      },
      async join() {
        p.log.push("join");
      },
      async clearState() {
        p.log.push("clear");
        p.state = null;
      },
      now: () => now,
    };
    return p;
  }

  it("drops the state, publishes its KeyPackage, knocks, joins from the Welcome", async () => {
    const p = ports(0);
    expect(await rejoinOnce(p, true)).toBe("joined");
    expect(p.log).toEqual(["drop", "directory-kp", "knock", "join", "clear"]);
  });

  it("waits, then knocks again with a fresh KeyPackage after the re-knock time", async () => {
    const p = ports(1);
    expect(await rejoinOnce(p, true)).toBe("waiting");
    expect(p.log).toEqual(["drop", "directory-kp", "knock"]);
    expect(await rejoinOnce(p, false)).toBe("joined");
    expect(p.log.filter((l) => l === "knock")).toHaveLength(2);
  });

  it("a reload resumes the knock it already made", async () => {
    const p = ports(0);
    p.state = { v: 1, rendezvousB64: "kept", joinerSnapshotB64: "s", knockedAtMs: 1_000 };
    expect(await rejoinOnce(p, false)).toBe("joined");
    expect(p.log).toEqual(["join", "clear"]);
  });

  it("the re-knock rule and the state codec", () => {
    const s: RejoinState = { v: 1, rendezvousB64: "r", joinerSnapshotB64: "s", knockedAtMs: 10 };
    expect(shouldKnockAgain(null, 0)).toBe(true);
    expect(shouldKnockAgain(s, 10 + REKNOCK_AFTER_MS - 1)).toBe(false);
    expect(shouldKnockAgain(s, 10 + REKNOCK_AFTER_MS)).toBe(true);
    expect(shouldKnockAgain(s, 5)).toBe(true); // a clock that went back
    expect(decodeRejoinState(encodeRejoinState(s))).toEqual(s);
    expect(decodeRejoinState(new TextEncoder().encode("[]"))).toBeNull();
  });
});
