// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  PASSKEY_PRF_SALT,
  passkeyAddOutcome,
  passkeyKeysLabel,
  passkeyKeysOutcome,
} from "./passkey-vault";

describe("the passkey PRF salt", () => {
  it("is the fixed string every Lakko client evaluates", () => {
    // iOS and Android must evaluate the same bytes, or a blob stored on one
    // client does not open on another.
    expect(new TextDecoder().decode(PASSKEY_PRF_SALT)).toBe("lakko/passkey-vault/prf/v1");
  });
});

describe("passkeyKeysLabel", () => {
  it("says a passkey restores keys exactly when keys are stored for it", () => {
    expect(passkeyKeysLabel("abc", ["abc", "def"])).toBe("restores-keys");
    expect(passkeyKeysLabel("zzz", ["abc", "def"])).toBe("signin-only");
    expect(passkeyKeysLabel("abc", [])).toBe("signin-only");
  });

  it("reads sign-in only while the server cannot say", () => {
    expect(passkeyKeysLabel("abc", null)).toBe("signin-only");
  });
});

describe("passkeyKeysOutcome", () => {
  it("restores only when the stored keys opened and matched the account", () => {
    expect(
      passkeyKeysOutcome({ prfOutput: true, blobStored: true, opened: true, matched: true }),
    ).toBe("restored");
  });

  it("is sign-in only without what protects the keys, or with nothing stored", () => {
    expect(
      passkeyKeysOutcome({ prfOutput: false, blobStored: false, opened: false, matched: false }),
    ).toBe("no-keys");
    expect(
      passkeyKeysOutcome({ prfOutput: true, blobStored: false, opened: false, matched: false }),
    ).toBe("no-keys");
  });

  it("is unusable, never restored, when stored keys do not open or do not match", () => {
    expect(
      passkeyKeysOutcome({ prfOutput: true, blobStored: true, opened: false, matched: false }),
    ).toBe("unusable");
    expect(
      passkeyKeysOutcome({ prfOutput: true, blobStored: true, opened: true, matched: false }),
    ).toBe("unusable");
  });

  it("never reports restored without a match, whatever else is claimed", () => {
    for (const prfOutput of [true, false]) {
      for (const blobStored of [true, false]) {
        for (const opened of [true, false]) {
          expect(passkeyKeysOutcome({ prfOutput, blobStored, opened, matched: false })).not.toBe(
            "restored",
          );
        }
      }
    }
  });
});

describe("passkeyAddOutcome", () => {
  it("stores the keys when the registration yielded the secret", () => {
    expect(passkeyAddOutcome({ prfOutput: true, prfEnabled: null })).toBe("store-keys");
  });

  it("stores the keys when the secret comes at the next use", () => {
    expect(passkeyAddOutcome({ prfOutput: false, prfEnabled: true })).toBe("store-keys");
  });

  it("asks the passkey once more when the browser said nothing about PRF", () => {
    expect(passkeyAddOutcome({ prfOutput: false, prfEnabled: null })).toBe("store-keys");
  });

  it("is sign-in only only when the browser said the passkey cannot carry keys", () => {
    expect(passkeyAddOutcome({ prfOutput: false, prfEnabled: false })).toBe("signin-only");
  });

  it("stores the keys when the registration yielded them, whatever the flag said", () => {
    expect(passkeyAddOutcome({ prfOutput: true, prfEnabled: false })).toBe("store-keys");
  });
});
