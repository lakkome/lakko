// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0079 amendment of 2026-10-19 - a link refusal removes B's pending
 * contact only when the inviter's account key signed it over the link B
 * answered and B's account key. Through the real core (`composeLinkRefusal`,
 * `verifyLinkRefusal`).
 */

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { Identity, composeLinkRefusal, verifyLinkRefusal } from "../../wasm/lakko_ffi_wasm";
import { pendingContactRefusedBy, type PendingContact } from "./pending-contacts";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const b64 = (bytes: Uint8Array): string => btoa(String.fromCharCode(...bytes));
const hex = (bytes: Uint8Array): string => Array.from(bytes, (x) => x.toString(16).padStart(2, "0")).join("");

describe("a link refusal (ADR-0079, 2026-10-19)", () => {
  it("removes the pending contact it names, and nothing else may", () => {
    const anna = Identity.fromSeed(new Uint8Array(32).fill(1));
    const mallory = Identity.fromSeed(new Uint8Array(32).fill(2));
    const bobKey = new Uint8Array(32).fill(0xb0);
    const linkId = new Uint8Array(16).fill(7);
    const pending: PendingContact = {
      accountIdHex: "a".repeat(32),
      signaturePubkeyB64: b64(anna.signaturePublicKey),
      x25519PubkeyB64: b64(anna.x25519PublicKey),
      displayName: "Anna",
      username: "anna",
      addedAtMs: 1,
      linkIdHex: hex(linkId),
    };
    const verify = (w: Uint8Array, k: Uint8Array, l: Uint8Array, o: Uint8Array): boolean => verifyLinkRefusal(w, k, l, o);
    const fromAnna = composeLinkRefusal(anna.ed25519Seed, linkId, bobKey);
    expect(pendingContactRefusedBy([pending], fromAnna, bobKey, verify)?.accountIdHex).toBe(pending.accountIdHex);
    // Not signed by the key the link named: ignored.
    const fromMallory = composeLinkRefusal(mallory.ed25519Seed, linkId, bobKey);
    expect(pendingContactRefusedBy([pending], fromMallory, bobKey, verify)).toBeNull();
    // For another link, or another person: ignored.
    expect(pendingContactRefusedBy([pending], composeLinkRefusal(anna.ed25519Seed, new Uint8Array(16).fill(8), bobKey), bobKey, verify)).toBeNull();
    expect(pendingContactRefusedBy([pending], fromAnna, new Uint8Array(32).fill(0xb1), verify)).toBeNull();
    anna.free();
    mallory.free();
  });
});
