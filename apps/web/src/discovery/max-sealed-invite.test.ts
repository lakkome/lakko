// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// The client's `/i/` ceiling must be the length of the longest sealed blob the
// core composes. A hand-copied number drifted twice (the link grant of
// ADR-0060, then its mailbox binding of ADR-0078) and refused every web invite
// link before the upload; this composes a maximal capability (the longest
// label and the longest inviter preview of ADR-0079) through the wasm and
// compares.

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, {
  Identity,
  InviteCapability,
  ReachClass,
  deriveReachGrant,
  generateLinkId,
} from "../../wasm/lakko_ffi_wasm";
import { MAX_SEALED_INVITE_BYTES } from "./client";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("MAX_SEALED_INVITE_BYTES", () => {
  it("is the sealed length of a capability with the longest label and preview", () => {
    const seed = new Uint8Array(32).fill(0x11);
    const identity = Identity.fromSeed(seed);
    const linkId = generateLinkId();
    const grant = deriveReachGrant(identity.ed25519Seed, ReachClass.Link, linkId, 0);
    const mint = InviteCapability.compose(
      identity.ed25519Seed,
      new Uint8Array(16).fill(0x22),
      identity.x25519PublicKey,
      0n,
      1,
      false,
      "l".repeat(64),
      linkId,
      grant,
      // 63 four-byte scalars and one three-byte scalar: 255 bytes in 64
      // scalars, the longest name both display-name bounds admit.
      `${"\u{1F600}".repeat(63)}\u{20AC}`,
      "a".repeat(32),
    );
    expect(mint.sealed.length).toBe(MAX_SEALED_INVITE_BYTES);
  });
});
