// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 walk 2026-11-26 (D4) - a received group name carries the role
 * table as it stood at receipt, the epoch the name was sealed at, so a commit
 * later in the same poll batch (an ownership hand-over) cannot take back a
 * name its sender was entitled to set. The same rule as iOS (`atReceipt`).
 *
 * The wasm is the REAL core (`composeGroupNameEnvelope`, the sniffer
 * `toEvent` runs); only the processed MLS frame and the group are literals.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmIfLoaded: () => wasm }));

import { roleTableAtReceipt, toEvent, type EvidenceGroup } from "./client";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const OWNER = new Uint8Array(32).fill(7);
const ADMIN = new Uint8Array(32).fill(9);

function group(owner: Uint8Array | undefined, admins: readonly Uint8Array[]): EvidenceGroup & { owner: Uint8Array | undefined } {
  const g = {
    groupId: new Uint8Array(32).fill(1),
    owner,
    deviceBindingAt: () => undefined,
    roleTableOwner: () => g.owner,
    roleTableAdmins: () => {
      const flat = new Uint8Array(admins.length * 32);
      admins.forEach((a, i) => flat.set(a, i * 32));
      return flat;
    },
  };
  return g;
}

describe("roleTableAtReceipt", () => {
  it("reads the owner and the admins now", () => {
    const t = roleTableAtReceipt(group(OWNER, [ADMIN]));
    expect(t?.ownerKey).toEqual(OWNER);
    expect(t?.adminKeys).toEqual([ADMIN]);
    expect(t?.verifyFailed).toBe(false);
  });

  it("a group with no table reader gives nothing; a reader that throws fails the verdict", () => {
    expect(roleTableAtReceipt(undefined)).toBeUndefined();
    expect(roleTableAtReceipt({ groupId: new Uint8Array(32), deviceBindingAt: () => undefined })).toBeUndefined();
    const broken: EvidenceGroup = {
      groupId: new Uint8Array(32),
      deviceBindingAt: () => undefined,
      roleTableOwner: () => {
        throw new Error("no table");
      },
      roleTableAdmins: () => new Uint8Array(0),
    };
    expect(roleTableAtReceipt(broken)?.verifyFailed).toBe(true);
  });
});

describe("a received group name", () => {
  it("keeps the table of its own epoch, whatever the group says later", async () => {
    const g = group(OWNER, []);
    const envelope = wasm.composeGroupNameEnvelope("Reset group", 1_000n);
    const e = await toEvent(
      5,
      "2026-11-26T10:00:00.000Z",
      { kind: "Application", applicationData: envelope, senderLeaf: 0 } as never,
      () => "c2VuZGVy",
      undefined,
      g,
    );
    // The hand-over lands later in the batch.
    g.owner = new Uint8Array(32).fill(3);
    expect(e.kind).toBe("group-name");
    if (e.kind !== "group-name") return;
    expect(e.atReceipt?.ownerKey).toEqual(OWNER);
    expect(e.atReceipt?.verifyFailed).toBe(false);
  });
});
