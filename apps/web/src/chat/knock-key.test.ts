// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 section 3, route 3 - the group's knock key: kept only from the
 * owner or an admin and only when newer (real core codec), and sent by the
 * owner on a version change or growth.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { bindContactsAccount } from "../contacts/local-store";
import { consumeKnockKeyRow, sendKnockKeyIfDue, KNOCK_KEY_ENVELOPE_LEN } from "./knock-key";
import { heldKnockKey, knockKeySendDue, knockKeySentMarkOf } from "./reentry-store";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const OWNER = new Uint8Array(32).fill(1);
const ADMIN = new Uint8Array(32).fill(2);
const MEMBER = new Uint8Array(32).fill(3);
const b64 = (b: Uint8Array): string => btoa(String.fromCharCode(...b));
const GROUP = {
  groupId: new Uint8Array([8, 8]),
  roleTableOwner: () => OWNER,
  roleTableAdmins: () => ADMIN,
} as unknown as WasmGroup;
const GID = b64(new Uint8Array([8, 8]));

let data: Map<string, string>;
beforeEach(() => {
  data = new Map();
  (globalThis as unknown as { localStorage: Storage }).localStorage = {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (k: string) => data.get(k) ?? null,
    key: (i: number) => [...data.keys()][i] ?? null,
    removeItem: (k: string) => void data.delete(k),
    setItem: (k: string, v: string) => void data.set(k, String(v)),
  };
  bindContactsAccount("a25vY2sta2V5");
});
afterEach(() => {
  bindContactsAccount(null);
  vi.restoreAllMocks();
});

function row(version: number, sender: Uint8Array) {
  const envelope = wasm.composeKnockKeyEnvelope(version, new Uint8Array(32).fill(version));
  expect(envelope.length).toBe(KNOCK_KEY_ENVELOPE_LEN);
  return { processed: { kind: "Application", applicationData: envelope, senderLeaf: 0 }, resolve: () => b64(sender) };
}

describe("receipt", () => {
  it("keeps a key from the owner or an admin, only when newer", async () => {
    const r1 = row(1, OWNER);
    expect(await consumeKnockKeyRow(GROUP, r1.processed, r1.resolve)).toBe(true);
    expect(heldKnockKey(GID)?.version).toBe(1);
    const r3 = row(3, ADMIN);
    await consumeKnockKeyRow(GROUP, r3.processed, r3.resolve);
    expect(heldKnockKey(GID)?.version).toBe(3);
    const r2 = row(2, OWNER);
    await consumeKnockKeyRow(GROUP, r2.processed, r2.resolve);
    expect(heldKnockKey(GID)?.version).toBe(3);
  });

  it("drops one from a plain member, and is consumed all the same (never shown)", async () => {
    const r = row(5, MEMBER);
    expect(await consumeKnockKeyRow(GROUP, r.processed, r.resolve)).toBe(true);
    expect(heldKnockKey(GID)).toBeUndefined();
  });

  it("any other row is not a knock key", async () => {
    expect(await consumeKnockKeyRow(GROUP, { kind: "Application", applicationData: new Uint8Array(60) }, () => b64(OWNER))).toBe(false);
    expect(await consumeKnockKeyRow(GROUP, { kind: "Commit" }, () => b64(OWNER))).toBe(false);
  });
});

describe("the owner's send", () => {
  it("is due on a new version and for a member the last delivery did not reach, not on a shrink", () => {
    const a = b64(new Uint8Array(32).fill(1));
    const b = b64(new Uint8Array(32).fill(2));
    const c = b64(new Uint8Array(32).fill(3));
    const d = b64(new Uint8Array(32).fill(4));
    const mark = knockKeySentMarkOf(1, [a, b, c]);
    expect(knockKeySendDue(undefined, 1, [a, b, c])).toBe(true);
    expect(knockKeySendDue(mark, 1, [c, a, b])).toBe(false);
    expect(knockKeySendDue(mark, 2, [a, b, c])).toBe(true);
    expect(knockKeySendDue(mark, 1, [a, b, c, d])).toBe(true);
    expect(knockKeySendDue(mark, 1, [a, b])).toBe(false);
    // A remove and an add in one commit: the count is unchanged.
    expect(knockKeySendDue(mark, 1, [a, b, d])).toBe(true);
  });

  it("sends once, then again after growth", async () => {
    const sent: Uint8Array[] = [];
    let members = [1, 2, 3].map((n) => b64(new Uint8Array(32).fill(n)));
    const ports = {
      ownerKnockKey: async () => ({ version: 1, publicKey: new Uint8Array(32).fill(7) }),
      memberKeys: () => members,
      compose: (v: number, pk: Uint8Array) => wasm.composeKnockKeyEnvelope(v, pk),
      broadcast: async (_g: WasmGroup, e: Uint8Array) => void sent.push(e),
    };
    expect(await sendKnockKeyIfDue(GROUP, ports)).toBe("sent");
    expect(await sendKnockKeyIfDue(GROUP, ports)).toBe("not-due");
    members = [...members, b64(new Uint8Array(32).fill(4))];
    expect(await sendKnockKeyIfDue(GROUP, ports)).toBe("sent");
    expect(sent.length).toBe(2);
    expect(await sendKnockKeyIfDue(GROUP, { ...ports, ownerKnockKey: async () => null })).toBe("not-owner");
  });

  it("a growth seen while the send failed stays due until a delivery reaches the new member", async () => {
    const sent: Uint8Array[] = [];
    let fail = false;
    let members = [1, 2].map((n) => b64(new Uint8Array(32).fill(n)));
    const ports = {
      ownerKnockKey: async () => ({ version: 3, publicKey: new Uint8Array(32).fill(9) }),
      memberKeys: () => members,
      compose: (v: number, pk: Uint8Array) => wasm.composeKnockKeyEnvelope(v, pk),
      broadcast: async (_g: WasmGroup, e: Uint8Array) => {
        if (fail) throw new Error("behind");
        sent.push(e);
      },
    };
    expect(await sendKnockKeyIfDue(GROUP, ports)).toBe("sent");
    members = [...members, b64(new Uint8Array(32).fill(5))];
    fail = true;
    expect(await sendKnockKeyIfDue(GROUP, ports)).toBe("failed");
    fail = false;
    expect(await sendKnockKeyIfDue(GROUP, ports)).toBe("sent");
    expect(await sendKnockKeyIfDue(GROUP, ports)).toBe("not-due");
    expect(sent.length).toBe(2);
  });
});
