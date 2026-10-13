// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A session names a device (`session-device.ts`, ADR-0074 section 0.4):
 * what a landing sends, when a session is bound afterwards, what a refused
 * bind means, and the genesis-recorded rule (section 0.1).
 */
import { describe, expect, it } from "vitest";

import {
  bindDecision,
  bindRefusal,
  clearGenesisRecorded,
  clearKnownDeviceId,
  deviceIdB64Url,
  genesisMintNeeded,
  genesisMintOutcome,
  isDeviceIdB64Url,
  landingDeviceId,
  readGenesisRecorded,
  readKnownDeviceId,
  tokenDeviceSegment,
  UNBOUND_DEVICE_ID_B64URL,
  writeGenesisRecorded,
  writeKnownDeviceId,
} from "./session-device";
import type { MarkerStorage } from "./phrase-gate";

function memory(): MarkerStorage & { readonly rows: Map<string, string> } {
  const rows = new Map<string, string>();
  return {
    rows,
    getItem: (k) => rows.get(k) ?? null,
    setItem: (k, v) => void rows.set(k, v),
    removeItem: (k) => void rows.delete(k),
  };
}

const DEV_A = deviceIdB64Url(new Uint8Array(32).fill(0xa1));
const DEV_B = deviceIdB64Url(new Uint8Array(32).fill(0xb2));

describe("deviceIdB64Url", () => {
  it("is 43 url-safe characters without padding", () => {
    expect(DEV_A).toHaveLength(43);
    expect(isDeviceIdB64Url(DEV_A)).toBe(true);
    expect(deviceIdB64Url(new Uint8Array(32).fill(0xff))).not.toMatch(/[+/=]/);
  });

  it("refuses anything that is not 32 bytes", () => {
    expect(() => deviceIdB64Url(new Uint8Array(31))).toThrow();
    expect(() => deviceIdB64Url(new Uint8Array(33))).toThrow();
  });

  it("encodes 32 zero bytes as the unbound id", () => {
    expect(deviceIdB64Url(new Uint8Array(32))).toBe(UNBOUND_DEVICE_ID_B64URL);
  });
});

describe("the id recorded for an account", () => {
  it("round-trips, per account, and is forgotten by the wipe", () => {
    const s = memory();
    writeKnownDeviceId(s, "acct-1", DEV_A);
    writeKnownDeviceId(s, "acct-2", DEV_B);
    expect(readKnownDeviceId(s, "acct-1")).toBe(DEV_A);
    expect(readKnownDeviceId(s, "acct-2")).toBe(DEV_B);
    clearKnownDeviceId(s, "acct-1");
    expect(readKnownDeviceId(s, "acct-1")).toBeNull();
    expect(readKnownDeviceId(s, "acct-2")).toBe(DEV_B);
  });

  it("reads a malformed or all-zero row as none", () => {
    const s = memory();
    s.rows.set("lakko:device-id:acct", "not an id");
    expect(readKnownDeviceId(s, "acct")).toBeNull();
    s.rows.set("lakko:device-id:acct", UNBOUND_DEVICE_ID_B64URL);
    expect(readKnownDeviceId(s, "acct")).toBeNull();
    expect(readKnownDeviceId(undefined, "acct")).toBeNull();
  });
});

describe("landingDeviceId: what a landing sends", () => {
  const known = (rows: Record<string, string>) => (account: string) => rows[account] ?? null;
  const cached = { accountIdB64: "acct-1", username: "Alice" };

  it("sends the id when the sign-in by name is for the cached account", () => {
    expect(
      landingDeviceId({
        cached,
        target: { username: " alice " },
        knownDeviceId: known({ "acct-1": DEV_A }),
      }),
    ).toBe(DEV_A);
  });

  it("sends nothing for another name: its account's device identity is not this one", () => {
    expect(
      landingDeviceId({
        cached,
        target: { username: "bob" },
        knownDeviceId: known({ "acct-1": DEV_A }),
      }),
    ).toBeUndefined();
  });

  it("sends nothing on a browser that remembers no account, or no id for it", () => {
    expect(
      landingDeviceId({ cached: null, target: { username: "alice" }, knownDeviceId: known({}) }),
    ).toBeUndefined();
    expect(
      landingDeviceId({ cached, target: { username: "alice" }, knownDeviceId: known({}) }),
    ).toBeUndefined();
  });

  it("sends nothing when the flow knows neither the account nor a name (discoverable passkey)", () => {
    expect(
      landingDeviceId({ cached, target: {}, knownDeviceId: known({ "acct-1": DEV_A }) }),
    ).toBeUndefined();
  });

  it("goes by the account id when the flow has it, whatever the cached marker says", () => {
    expect(
      landingDeviceId({
        cached,
        target: { accountIdB64: "acct-2" },
        knownDeviceId: known({ "acct-1": DEV_A, "acct-2": DEV_B }),
      }),
    ).toBe(DEV_B);
    expect(
      landingDeviceId({
        cached,
        target: { accountIdB64: "acct-3", username: "alice" },
        knownDeviceId: known({ "acct-1": DEV_A }),
      }),
    ).toBeUndefined();
  });

  it("never matches an empty cached username (a guest account)", () => {
    expect(
      landingDeviceId({
        cached: { accountIdB64: "acct-1", username: "" },
        target: { username: "" },
        knownDeviceId: known({ "acct-1": DEV_A }),
      }),
    ).toBeUndefined();
  });
});

describe("binding afterwards", () => {
  const token = (device: string, version = "v3"): string =>
    `${version}.YWNjdA.1.${device}.1700000000000.1700000900000.dGFn`;

  it("reads the device segment of a v3 token and an r2 credential only", () => {
    expect(tokenDeviceSegment(token(DEV_A))).toBe(DEV_A);
    expect(tokenDeviceSegment(token(DEV_A, "r2"))).toBe(DEV_A);
    expect(tokenDeviceSegment(token(DEV_A, "v2"))).toBeNull();
    expect(tokenDeviceSegment("v2.acct.1.exp.tag")).toBeNull();
    expect(tokenDeviceSegment("k1.acct.1.exp.tag")).toBeNull();
    expect(tokenDeviceSegment(token("short"))).toBeNull();
    expect(tokenDeviceSegment("")).toBeNull();
  });

  it("does nothing for a session that already names this device", () => {
    expect(bindDecision({ sessionToken: token(DEV_A), ownDeviceIdB64: DEV_A })).toBe("bound");
  });

  it("binds a session that names no device, another device, or cannot be read", () => {
    expect(
      bindDecision({ sessionToken: token(UNBOUND_DEVICE_ID_B64URL), ownDeviceIdB64: DEV_A }),
    ).toBe("bind");
    expect(bindDecision({ sessionToken: token(DEV_B), ownDeviceIdB64: DEV_A })).toBe("bind");
    expect(bindDecision({ sessionToken: "opaque", ownDeviceIdB64: DEV_A })).toBe("bind");
  });

  it("binds regardless of the hint when forced (discovery said device-mismatch)", () => {
    expect(
      bindDecision({ sessionToken: token(DEV_A), ownDeviceIdB64: DEV_A, force: true }),
    ).toBe("bind");
  });

  it("tells the refusals of bind-device apart", () => {
    expect(bindRefusal(401, "device-revoked")).toBe("device-revoked");
    expect(bindRefusal(401, "key-generation-superseded")).toBe("superseded");
    expect(bindRefusal(401, "refresh-invalid")).toBe("session-ended");
    expect(bindRefusal(401, null)).toBe("session-ended");
    expect(bindRefusal(409, "device-already-bound")).toBe("sign-in-again");
  });

  it("never ends a session on an answer that is not a verdict", () => {
    expect(bindRefusal(400, null)).toBe("later");
    expect(bindRefusal(409, null)).toBe("later");
    expect(bindRefusal(429, "too-many-attempts")).toBe("later");
    expect(bindRefusal(500, null)).toBe("later");
    expect(bindRefusal(503, "rate-limiter-unavailable")).toBe("later");
    // A 403 carrying the removal code is not auth's 401.
    expect(bindRefusal(403, "device-revoked")).toBe("later");
  });
});

describe("the genesis-recorded rule", () => {
  it("mints on every landing until it succeeded once for this key", () => {
    const s = memory();
    expect(genesisMintNeeded(readGenesisRecorded(s, "acct"), "key-1")).toBe(true);
    writeGenesisRecorded(s, "acct", "key-1");
    expect(genesisMintNeeded(readGenesisRecorded(s, "acct"), "key-1")).toBe(false);
  });

  it("mints again after a key reset (the mark names the old key)", () => {
    const s = memory();
    writeGenesisRecorded(s, "acct", "key-1");
    expect(genesisMintNeeded(readGenesisRecorded(s, "acct"), "key-2")).toBe(true);
  });

  it("keeps the mark per account and drops it with the wipe", () => {
    const s = memory();
    writeGenesisRecorded(s, "acct-1", "key-1");
    expect(readGenesisRecorded(s, "acct-2")).toBeNull();
    clearGenesisRecorded(s, "acct-1");
    expect(readGenesisRecorded(s, "acct-1")).toBeNull();
  });

  it("marks only a mint that auth answered", () => {
    expect(genesisMintOutcome({ ok: true })).toBe("recorded");
    expect(genesisMintOutcome({ ok: false, status: 409, errorCode: "identity-key-mismatch" })).toBe(
      "mismatch",
    );
    expect(genesisMintOutcome({ ok: false, status: 409, errorCode: null })).toBe("retry");
    expect(genesisMintOutcome({ ok: false, status: 400 })).toBe("retry");
    expect(genesisMintOutcome({ ok: false, status: 503 })).toBe("retry");
    expect(genesisMintOutcome({ ok: false })).toBe("retry");
  });
});
