// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The DeviceGroup filter (ADR-0074 section 2.3). */
import { afterEach, describe, expect, it } from "vitest";

import {
  hexOfKey,
  ownDeviceAdmissible,
  ownDeviceRoster,
  publishOwnDeviceRoster,
} from "./own-device-roster";

afterEach(() => publishOwnDeviceRoster(null));

describe("ownDeviceAdmissible", () => {
  it("admits a device the roster names, in either hex case", () => {
    publishOwnDeviceRoster({ accountIdB64: "acct", keysHex: ["AB01", "cd02"] });
    const roster = ownDeviceRoster("acct");
    expect(ownDeviceAdmissible(roster, "ab01")).toBe(true);
    expect(ownDeviceAdmissible(roster, "CD02")).toBe(true);
  });

  it("refuses a device the roster does not name", () => {
    publishOwnDeviceRoster({ accountIdB64: "acct", keysHex: ["ab01"] });
    expect(ownDeviceAdmissible(ownDeviceRoster("acct"), "ff00")).toBe(false);
  });

  it("fails closed while no roster is known", () => {
    expect(ownDeviceAdmissible(null, "ab01")).toBe(false);
    expect(ownDeviceAdmissible(ownDeviceRoster("acct"), "ab01")).toBe(false);
  });

  it("never answers with another account's roster", () => {
    publishOwnDeviceRoster({ accountIdB64: "other", keysHex: ["ab01"] });
    expect(ownDeviceRoster("acct")).toBeNull();
    expect(ownDeviceAdmissible(ownDeviceRoster("acct"), "ab01")).toBe(false);
  });

  it("forgets the roster when it is withdrawn (sign-out, this device removed)", () => {
    publishOwnDeviceRoster({ accountIdB64: "acct", keysHex: ["ab01"] });
    publishOwnDeviceRoster(null);
    expect(ownDeviceRoster("acct")).toBeNull();
  });
});

describe("hexOfKey", () => {
  it("is lowercase, two digits per byte", () => {
    expect(hexOfKey(new Uint8Array([0, 1, 0xab, 0xff]))).toBe("0001abff");
  });
});
