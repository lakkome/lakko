// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0054 decision 1 / ADR-0078 section 3 — the `x-lakko-wake` header value.
 * The DS answers 400 to a malformed header, which fails the publish itself,
 * so the helper that puts it on the wire checks its shape: a wake sealed to
 * push-gw, 312 base64url characters. The sealed value comes from the REAL
 * wasm core here, and the wake derivations are pinned to the Rust goldens.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { DS_WAKE_HEADER, DS_WAKE_HEADER_LEN, DsWakeArgError, dsWakeHeaderValue } from "./wake";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

describe("dsWakeHeaderValue", () => {
  it("is named `x-lakko-wake`, as the core says", () => {
    expect(DS_WAKE_HEADER).toBe("x-lakko-wake");
    expect(wasm.pushWakeHeaderName()).toBe(DS_WAKE_HEADER);
  });

  it("passes a sealed wake from the core through unchanged", () => {
    const gwKey = new Uint8Array(32).fill(0x09);
    const secret = new Uint8Array(32).fill(0x42);
    for (const kind of [wasm.PushWakeKind.Message, wasm.PushWakeKind.Call]) {
      const value = wasm.pushWakeHeaderValue(gwKey, secret, kind, Date.now());
      expect(value).toHaveLength(DS_WAKE_HEADER_LEN);
      expect(value).toMatch(/^[A-Za-z0-9_-]+$/);
      expect(dsWakeHeaderValue({ headerValue: value })).toBe(value);
    }
  });

  it("never carries the push_gid or the kind in the clear", () => {
    const secret = new Uint8Array(32).fill(0x42);
    const value = wasm.pushWakeHeaderValue(new Uint8Array(32).fill(1), secret, wasm.PushWakeKind.Call, 1);
    const gidB64 = Buffer.from(wasm.derivePushGid(secret)).toString("base64url");
    expect(value).not.toContain(gidB64.slice(0, 20));
    expect(value).not.toContain("call");
    // Two wakes for the same group are never equal.
    const again = wasm.pushWakeHeaderValue(new Uint8Array(32).fill(1), secret, wasm.PushWakeKind.Call, 1);
    expect(again).not.toBe(value);
  });

  it("pins the wake derivations to the Rust goldens", () => {
    const secret = new Uint8Array(32).fill(0x42);
    expect(hex(wasm.derivePushWakePublicKey(secret))).toBe(
      "2153e575c8abc4a91af2b70258982068480edc0e0451f6b6bdf6c0692e5c8e56",
    );
    expect(hex(wasm.derivePushWakeUnlockKey(secret))).toBe(
      "d76a3ce72be511a512c6afce89447fa73d9595c5b0fe0942780137a7d1f4cd0a",
    );
  });

  it("refuses a value of the wrong shape, including the ADR-0054 spelling", () => {
    for (const bad of [
      "",
      "a".repeat(DS_WAKE_HEADER_LEN - 1),
      "a".repeat(DS_WAKE_HEADER_LEN + 1),
      "+".repeat(DS_WAKE_HEADER_LEN),
      `${"A".repeat(43)};message`,
    ]) {
      expect(() => dsWakeHeaderValue({ headerValue: bad })).toThrow(DsWakeArgError);
    }
  });
});
