// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The person's own part of a backup (D31, D37), through the real core: the
 * payload build and parse, the profile and visibility envelopes, and the rule
 * that nothing newer is overwritten and nothing unreadable changes anything.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { WebDeviceCipher } from "../auth/device-cipher";
import { putDeviceKey } from "../auth/key-store";
import { loadOwnProfile, saveOwnProfile } from "../profile/own-profile";
import { loadReachRow, saveReachRow } from "../reach/reach-store";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import type { Wasm } from "../wasm";
import {
  adoptRestoredOwnProfile,
  decodeRestoredOwnProfile,
  ownProfileForBackup,
  registerBackupUsernameSource,
  restoredOwnUsername,
  storeRestoredVisibility,
  visibilityForBackup,
  visibilityStampOf,
} from "./own-state";

const core = wasm as unknown as Wasm;
const ACCOUNT = "QkJCQkJCQkJCQkJCQkJCQg==";
const ACCOUNT_BYTES = base64Decode(ACCOUNT);
const OTHER_ACCOUNT_FOR_USERNAME = "Q0NDQ0NDQ0NDQ0NDQ0NDQw==";

/** ADR-0101 section 8: a v6 profile envelope is signed by an account
 *  identity; this test's profiles are signed by one fixed identity. */
let profileSignerHeld: wasm.Identity | undefined;
function profileSigner(): wasm.Identity {
  return (profileSignerHeld ??= wasm.Identity.fromSeed(new Uint8Array(64).fill(0x5a)));
}

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

beforeEach(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
  await putDeviceKey(
    ACCOUNT,
    new WebDeviceCipher(
      await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]),
    ),
  );
});
afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

function visibilityAt(stamp: bigint): Uint8Array {
  const base = wasm.defaultVisibilitySetting();
  const chosen = wasm.chooseVisibilityMode(base, wasm.VisibilityMode.Everyone);
  const stamped = chosen.withUpdatedAtMs(stamp);
  return wasm.composeVisibilityEnvelope(stamped);
}

describe("what a backup carries", () => {
  it("the stored name under the stored stamp, with the username", () => {
    const own = saveOwnProfile(ACCOUNT, "Anna", 1_000);
    const stop = registerBackupUsernameSource(() => "anna");
    const bytes = ownProfileForBackup(core, profileSigner(), ACCOUNT, ACCOUNT_BYTES);
    stop();
    expect(bytes).not.toBeNull();
    const decoded = wasm.decodeProfileEnvelope(bytes as Uint8Array);
    expect(decoded.displayName).toBe("Anna");
    expect(decoded.username).toBe("anna");
    expect(decoded.updatedAtMs).toBe(BigInt(own.updatedAtMs));
  });

  it("nothing for an account that never saved a profile", () => {
    expect(ownProfileForBackup(core, profileSigner(), ACCOUNT, ACCOUNT_BYTES)).toBeNull();
  });

  it("travels through the payload and comes back", async () => {
    saveOwnProfile(ACCOUNT, "Anna", 1_000);
    const stored = visibilityAt(5n);
    await saveReachRow(ACCOUNT, "visibility", base64Encode(stored));
    const payload = wasm.buildBackupPayload(
      ACCOUNT_BYTES,
      1n,
      new Uint8Array(32).fill(3),
      [],
      new Uint8Array(0),
      undefined,
      undefined,
      undefined,
      ownProfileForBackup(core, profileSigner(), ACCOUNT, ACCOUNT_BYTES) ?? undefined,
      (await visibilityForBackup(ACCOUNT)) ?? undefined,
    );
    const parsed = wasm.parseBackupPayload(payload);
    expect(decodeRestoredOwnProfile(core, parsed.ownProfile, ACCOUNT)?.displayName).toBe("Anna");
    // No username source stood when it was composed: the backup names none.
    expect(decodeRestoredOwnProfile(core, parsed.ownProfile, ACCOUNT)?.username).toBeNull();
    expect(visibilityStampOf(core, parsed.visibility)).toBe(5n);
  });
});

describe("the username a backup names (docs/ux/identity-recovery.md 8b)", () => {
  it("is kept for the landing to check, apart from the name and photo", async () => {
    const stop = registerBackupUsernameSource(() => "anna");
    saveOwnProfile(ACCOUNT, "Anna", 1_000);
    const bytes = ownProfileForBackup(core, profileSigner(), ACCOUNT, ACCOUNT_BYTES) as Uint8Array;
    stop();
    expect(decodeRestoredOwnProfile(core, bytes, ACCOUNT)?.username).toBe("anna");
    expect(restoredOwnUsername(ACCOUNT)).toBeNull();
    await adoptRestoredOwnProfile(ACCOUNT, bytes);
    expect(restoredOwnUsername(ACCOUNT)).toBe("anna");
  });

  it("a profile with no username leaves nothing to check", async () => {
    const carried = wasm.composeProfileEnvelope(profileSigner(), "Anna", undefined, 500n, base64Decode(OTHER_ACCOUNT_FOR_USERNAME));
    expect(decodeRestoredOwnProfile(core, carried, OTHER_ACCOUNT_FOR_USERNAME)?.username).toBeNull();
    await adoptRestoredOwnProfile(OTHER_ACCOUNT_FOR_USERNAME, carried);
    expect(restoredOwnUsername(OTHER_ACCOUNT_FOR_USERNAME)).toBeNull();
  });
});

describe("restoring the name and photo", () => {
  it("fills an empty store under the stored stamp", async () => {
    const carried = wasm.composeProfileEnvelope(profileSigner(), "Anna", undefined, 500n, ACCOUNT_BYTES);
    expect(await adoptRestoredOwnProfile(ACCOUNT, carried)).toBe(true);
    expect(loadOwnProfile(ACCOUNT)).toMatchObject({ displayName: "Anna", updatedAtMs: 500 });
  });

  it("leaves a newer profile alone", async () => {
    saveOwnProfile(ACCOUNT, "Newer", 900);
    const carried = wasm.composeProfileEnvelope(profileSigner(), "Older", undefined, 500n, ACCOUNT_BYTES);
    expect(await adoptRestoredOwnProfile(ACCOUNT, carried)).toBe(false);
    expect(loadOwnProfile(ACCOUNT).displayName).toBe("Newer");
  });

  it("an empty, damaged or foreign field changes nothing", async () => {
    saveOwnProfile(ACCOUNT, "Kept", 900);
    const foreign = wasm.composeProfileEnvelope(profileSigner(), "Other", undefined, 2_000n, new Uint8Array(16).fill(9));
    expect(await adoptRestoredOwnProfile(ACCOUNT, new Uint8Array(0))).toBe(false);
    expect(await adoptRestoredOwnProfile(ACCOUNT, new Uint8Array([1, 2, 3]))).toBe(false);
    expect(await adoptRestoredOwnProfile(ACCOUNT, foreign)).toBe(false);
    expect(loadOwnProfile(ACCOUNT).displayName).toBe("Kept");
  });
});

describe("restoring the setting", () => {
  it("fills an empty row", async () => {
    const carried = visibilityAt(5n);
    expect(await storeRestoredVisibility(ACCOUNT, carried)).toBe(true);
    expect(await loadReachRow(ACCOUNT, "visibility")).toBe(base64Encode(carried));
  });

  it("leaves a newer setting in its row, and still answers that the field was one", async () => {
    const newer = base64Encode(visibilityAt(9n));
    await saveReachRow(ACCOUNT, "visibility", newer);
    expect(await storeRestoredVisibility(ACCOUNT, visibilityAt(5n))).toBe(true);
    expect(await loadReachRow(ACCOUNT, "visibility")).toBe(newer);
  });

  it("an empty or damaged field changes nothing and goes no further", async () => {
    expect(await storeRestoredVisibility(ACCOUNT, new Uint8Array(0))).toBe(false);
    expect(await storeRestoredVisibility(ACCOUNT, new Uint8Array([7, 7]))).toBe(false);
    expect(await loadReachRow(ACCOUNT, "visibility")).toBeNull();
  });
});
