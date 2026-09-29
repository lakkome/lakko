// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0058 profile v3 — the foreground sniff detaches the grants and the
 * username certificate from the wasm handle, and the fold keeps them under
 * the rules. Over the REAL core composer and decoder.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { sniffProfile } from "../profile/profile-envelope";
import { foldProfileGrants } from "./grant-intake";
import { EMPTY_RECEIVED_GRANTS, bestGrantFor } from "./received-grants";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const ACCOUNT_SEED = new Uint8Array(32).fill(0x42);
const GROUP = new Uint8Array(32).fill(0x07);

describe("profile v3 through the foreground sniff", () => {
  it("carries each grant as its 37-byte wire with class and version", () => {
    const member = wasm.deriveReachGrant(ACCOUNT_SEED, wasm.ReachClass.Member, GROUP, 2);
    const contact = wasm.deriveReachGrant(ACCOUNT_SEED, wasm.ReachClass.Contact, new Uint8Array(0), 1);
    // Read before composing: `composeProfileEnvelope` takes the grants by value.
    const memberKey = member.verifyingKey();
    const bytes = wasm.composeProfileEnvelope("Anna", undefined, 5n, new Uint8Array(16).fill(1), undefined, [
      member,
      contact,
    ]);
    const sniff = sniffProfile(wasm, bytes);
    expect(sniff.kind).toBe("profile");
    if (sniff.kind !== "profile") throw new Error("unreachable");
    const grants = sniff.profile.grants ?? [];
    expect(grants.map((g) => [g.classByte, g.version])).toEqual([
      [5, 2],
      [1, 1],
    ]);
    const back = wasm.decodeReachGrant(Uint8Array.from(atob(grants[0]?.wireB64 ?? ""), (c) => c.charCodeAt(0)));
    expect(back.verifyingKey()).toEqual(memberKey);
    // The handles were moved into the envelope: freeing one again throws,
    // which is why every caller frees them quietly.
    expect(() => member.free()).toThrow();
    expect(sniff.profile.usernameCertificate).toBeNull();
  });

  it("a one-to-one keeps the contact grant, which then authorises a write", () => {
    const contact = wasm.deriveReachGrant(ACCOUNT_SEED, wasm.ReachClass.Contact, new Uint8Array(0), 0);
    const bytes = wasm.composeProfileEnvelope("Anna", undefined, 5n, undefined, undefined, [contact]);
    const sniff = sniffProfile(wasm, bytes);
    if (sniff.kind !== "profile") throw new Error("not a profile");
    const store = foldProfileGrants(EMPTY_RECEIVED_GRANTS, {
      senderHex: "aa",
      ownHex: "00",
      scopeHex: "07".repeat(32),
      oneToOne: true,
      grants: sniff.profile.grants ?? [],
      nowMs: 1,
    });
    expect(bestGrantFor(store, "aa")?.version).toBe(0);
    const inGroup = foldProfileGrants(EMPTY_RECEIVED_GRANTS, {
      senderHex: "aa",
      ownHex: "00",
      scopeHex: "07".repeat(32),
      oneToOne: false,
      grants: sniff.profile.grants ?? [],
      nowMs: 1,
    });
    expect(bestGrantFor(inGroup, "aa")).toBeNull();
  });
});
