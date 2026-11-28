// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 item 4 - after a retake, a channel's `@name` moves
 * from the handle key its owned-channels list entry carried to the key the
 * new account's root derives (`rekeyChannelHandleFromSeed`).
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as realWasm from "../../wasm/lakko_ffi_wasm";

const sent = vi.hoisted(() => ({ calls: [] as { route: string; statement: string }[] }));

vi.mock("../discovery/handle-lookup", () => ({
  handleWrite: async (route: string, statement: string) => {
    sent.calls.push({ route, statement });
    return { leaseUntilMs: 7, certificate: new Uint8Array([1]) };
  },
  lookupHandle: async () => null,
}));
vi.mock("../discovery/handle-check", () => ({
  namePair: async (handle: string) => ({ normalized: handle, output: new Uint8Array(64), lookupKey: new Uint8Array(64).fill(4) }),
  noteHandleHeld: () => undefined,
  termPairsInBatch: async () => [],
}));

import { channelHandleSeed, rekeyChannelHandleFromSeed } from "./group-handle";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const CHANNEL_ID = new Uint8Array(16).fill(0x0c);

describe("rekeyChannelHandleFromSeed", () => {
  it("signs rekey with the listed seed to the new root's handle key", async () => {
    const old = realWasm.Identity.fromSeed(new Uint8Array(64).fill(1));
    const next = realWasm.Identity.fromSeed(new Uint8Array(64).fill(2));
    const oldSeed = channelHandleSeed(realWasm, old, old, CHANNEL_ID);
    const newSeed = channelHandleSeed(realWasm, next, next, CHANNEL_ID);
    const oldPk = realWasm.handleWritePublicKey(oldSeed);
    const newPk = realWasm.handleWritePublicKey(newSeed);
    const written = await rekeyChannelHandleFromSeed({
      wasm: realWasm,
      oldSeed: oldSeed.slice(),
      lineageIdentity: next,
      currentIdentity: next,
      channelId: CHANNEL_ID,
      handle: "news",
      isPublic: true,
    });
    expect(sent.calls.map((c) => c.route)).toEqual(["rekey"]);
    // `domain ‖ kind ‖ lookup(64) ‖ write_pk(32) ‖ ms ‖ nonce ‖ new_pk(32) ‖ sig(64)`:
    // signed by the listed (old) key, naming the new root's key.
    const raw = Uint8Array.from(atob(sent.calls[0]?.statement ?? ""), (c) => c.charCodeAt(0));
    const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
    expect(hex(raw.slice(raw.length - 96, raw.length - 64))).toBe(hex(newPk));
    expect(hex(raw)).toContain(hex(new Uint8Array(64).fill(4)) + hex(oldPk));
    expect(hex(oldPk)).not.toBe(hex(newPk));
    expect(written).toMatchObject({ handle: "news", isPublic: true, leaseUntilMs: 7 });
  });
});
