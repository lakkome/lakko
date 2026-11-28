// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 10, "The account's own devices": a reset made elsewhere
 * that the core judges sealed is the owner's (no "Was it you?"); unsealed,
 * refused or unreadable keeps the recall.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "./account-id";
import type { IdentityFetchResponse } from "../discovery/wire";
import type { ResetVerdict } from "../contacts/reset-verdict";
import { judgeOwnReset, type OwnResetPorts } from "./own-reset-verdict";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const ACCOUNT = new Uint8Array(16).fill(1);
const OLD = new Uint8Array(32).fill(2);
const NEW = new Uint8Array(32).fill(3);

function bundle(key: Uint8Array, generation: number): IdentityFetchResponse {
  return { signature_pubkey: base64Encode(key), key_generation: generation } as unknown as IdentityFetchResponse;
}

function ports(
  served: IdentityFetchResponse | null | "offline",
  verdict: ResetVerdict,
  seen: { accepted?: number; held?: string } = {},
): OwnResetPorts {
  return {
    fetchBundle: async () => {
      if (served === "offline") throw new Error("offline");
      return served;
    },
    judge: async (input) => {
      seen.accepted = input.acceptedGeneration;
      seen.held = input.heldKeyB64;
      return { verdict, log: { status: "off", isReset: null } };
    },
  };
}

const input = { accountId: ACCOUNT, heldKey: OLD, heldGeneration: 4, seqFloor: 0n };

describe("judgeOwnReset", () => {
  it("a sealed reset is the owner's", async () => {
    const seen: { accepted?: number; held?: string } = {};
    expect(
      await judgeOwnReset(input, ports(bundle(NEW, 5), { kind: "sealed", keyGeneration: 5, signedAtMs: 1, announcementSeq: 2n }, seen)),
    ).toBe("sealed");
    expect(seen).toEqual({ accepted: 4, held: base64Encode(OLD) });
  });

  it("unsealed and refused keep the recall; no verdict and no answer are unknown", async () => {
    expect(await judgeOwnReset(input, ports(bundle(NEW, 5), { kind: "unsealed", keyGeneration: 5, signedAtMs: 1 }))).toBe(
      "not-sealed",
    );
    expect(await judgeOwnReset(input, ports(bundle(NEW, 5), { kind: "refused", why: "unconfirmed" }))).toBe("not-sealed");
    expect(await judgeOwnReset(input, ports(bundle(NEW, 5), { kind: "no-verdict" }))).toBe("unknown");
    expect(await judgeOwnReset(input, ports("offline", { kind: "no-verdict" }))).toBe("unknown");
    expect(await judgeOwnReset(input, ports(null, { kind: "no-verdict" }))).toBe("unknown");
  });

  it("a directory that still names this device's key judges nothing", async () => {
    expect(
      await judgeOwnReset(input, ports(bundle(OLD, 4), { kind: "sealed", keyGeneration: 5, signedAtMs: 1, announcementSeq: 2n })),
    ).toBe("unknown");
  });

  it("without the session's generation, one below the published one", async () => {
    const seen: { accepted?: number } = {};
    await judgeOwnReset(
      { ...input, heldGeneration: null },
      ports(bundle(NEW, 7), { kind: "unsealed", keyGeneration: 7, signedAtMs: 1 }, seen),
    );
    expect(seen.accepted).toBe(6);
  });

  it("the recall record's 32-byte seed names the old account key (the key the check holds)", () => {
    const identity = wasm.Identity.fromSeed(new Uint8Array(64).fill(9));
    expect(Array.from(wasm.handleWritePublicKey(identity.ed25519Seed))).toEqual(Array.from(identity.signaturePublicKey));
  });
});
