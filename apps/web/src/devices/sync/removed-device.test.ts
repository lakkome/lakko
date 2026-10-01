// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";
import { takeSigner } from "../../ds/access";

import {
  REMOVED_DEVICE_COPY,
  forkRepairAllowed,
  isDeviceRemoved,
  pollStepAfterBatch,
  readDeviceGroupActive,
  type ActiveReadable,
} from "./removed-device";

const active: ActiveReadable = { isActive: true };
const inactive: ActiveReadable = { isActive: false };
const unreadable: ActiveReadable = {
  get isActive(): boolean {
    throw new Error("internal lock poisoned");
  },
};

describe("removed device (second-device.md section 3.1)", () => {
  it("reads the handle, and a failed read is unknown rather than removed", () => {
    expect(readDeviceGroupActive(active)).toBe(true);
    expect(readDeviceGroupActive(inactive)).toBe(false);
    expect(readDeviceGroupActive(unreadable)).toBeNull();
  });

  it("an inactive group is removed and stops the poll loop", () => {
    const read = readDeviceGroupActive(inactive);
    expect(isDeviceRemoved(read)).toBe(true);
    expect(pollStepAfterBatch(read)).toBe("stop-removed");
  });

  it("an active group is not removed and keeps polling", () => {
    const read = readDeviceGroupActive(active);
    expect(isDeviceRemoved(read)).toBe(false);
    expect(pollStepAfterBatch(read)).toBe("continue");
  });

  it("an unreadable group never shows the removed screen", () => {
    const read = readDeviceGroupActive(unreadable);
    expect(isDeviceRemoved(read)).toBe(false);
    expect(pollStepAfterBatch(read)).toBe("continue");
  });

  it("the fork repair runs only on a group known to be active", () => {
    expect(forkRepairAllowed(readDeviceGroupActive(active))).toBe(true);
    expect(forkRepairAllowed(readDeviceGroupActive(inactive))).toBe(false);
    expect(forkRepairAllowed(readDeviceGroupActive(unreadable))).toBe(false);
  });

  it("uses the spec's copy exactly", () => {
    expect(REMOVED_DEVICE_COPY.title).toBe("This device was removed");
    expect(REMOVED_DEVICE_COPY.body).toBe(
      "It was removed from your account on another device. New messages no longer reach it. Sign out, then sign in again if you want to use it.",
    );
    expect(REMOVED_DEVICE_COPY.signOut).toBe("Sign out");
  });
});

// The same rules over the real wasm DeviceGroup: a device that applies the
// commit removing its own leaf reads inactive, and the DS signer the fork
// check derives its epoch from cannot be derived from it.
describe("removed device over the real DeviceGroup", () => {
  const ACCOUNT = new Uint8Array(16).fill(0x5a);

  beforeAll(async () => {
    const wasmPath = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
    await init({ module_or_path: readFileSync(wasmPath) });
  });

  it("a removal stops the removed device and leaves the remover active", () => {
    const idA = wasm.Identity.fromSeed(new Uint8Array(32).fill(7));
    const idB = wasm.Identity.fromSeed(new Uint8Array(32).fill(8));
    const a = wasm.DeviceGroup.createFounder(idA, ACCOUNT);
    const added = a.addDeviceFromWire(idA, idB.createKeyPackage());
    const b = wasm.DeviceGroup.joinFromWelcome(added.welcome, idB, ACCOUNT);
    try {
      expect(readDeviceGroupActive(b)).toBe(true);
      expect(pollStepAfterBatch(readDeviceGroupActive(b))).toBe("continue");

      const commit = a.removeDeviceAt(idA, b.ownLeafIndex());
      b.processIncoming(idB, commit).free();

      expect(readDeviceGroupActive(b)).toBe(false);
      expect(pollStepAfterBatch(readDeviceGroupActive(b))).toBe("stop-removed");
      expect(forkRepairAllowed(readDeviceGroupActive(b))).toBe(false);
      expect(takeSigner(() => b.dsAccessSigner(idB))).toBeNull();

      expect(readDeviceGroupActive(a)).toBe(true);
      expect(forkRepairAllowed(readDeviceGroupActive(a))).toBe(true);
    } finally {
      added.free();
      a.free();
      b.free();
      idA.free();
      idB.free();
    }
  });
});
