// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Storing the keys for a passkey with nothing to choose
 * (docs/ux/identity-recovery.md 10.2, amended 2026-11-11): the outcomes of
 * `storeKeysForPasskeyWith` and the lines the add flow and the repair show
 * for each.
 */
import { describe, expect, it } from "vitest";

import { AuthHttpError, AuthNetworkError } from "./client";
import {
  addedPasskeyMessage,
  repairMessage,
  storeKeysForPasskeyWith,
  type StoreKeysDeps,
} from "./passkey-keys-store";
import { WebauthnNetworkError, WebauthnUserCancelledError } from "./webauthn-client";

const ACCOUNT_SEED = new Uint8Array(64).fill(7);
const OTHER_SEED = new Uint8Array(64).fill(9);
const CRED = "cred-a";
const PRF = new Uint8Array(32).fill(1);

interface Recorder {
  prompts: string[];
  enrolled: { prfOutput: Uint8Array; phrase: string }[];
}

function deps(over: Partial<StoreKeysDeps> = {}): { deps: StoreKeysDeps; rec: Recorder } {
  const rec: Recorder = { prompts: [], enrolled: [] };
  const base: StoreKeysDeps = {
    readDevicePhrase: () => Promise.resolve("own phrase"),
    phraseSeed: (phrase) => {
      if (phrase === "own phrase") return ACCOUNT_SEED.slice();
      if (phrase === "other phrase") return OTHER_SEED.slice();
      throw new Error("not a phrase");
    },
    accountSeed: ACCOUNT_SEED,
    evaluatePrf: (id) => {
      rec.prompts.push(id);
      return Promise.resolve({ credentialIdB64: id, prfOutput: PRF });
    },
    enroll: (prfOutput, phrase) => {
      rec.enrolled.push({ prfOutput, phrase });
      return Promise.resolve(true);
    },
  };
  return { deps: { ...base, ...over }, rec };
}

describe("storing the keys for a passkey just added", () => {
  it("stores them from the registration's PRF output without a second prompt", async () => {
    const { deps: d, rec } = deps();
    expect(await storeKeysForPasskeyWith(d, { credentialIdB64: CRED, prfOutput: PRF })).toBe(
      "stored",
    );
    expect(rec.prompts).toEqual([]);
    expect(rec.enrolled).toEqual([{ prfOutput: PRF, phrase: "own phrase" }]);
    expect(addedPasskeyMessage("stored").key).toBe("settings.passkeys.added_restores");
  });

  it("asks the passkey once more, for this credential only, when creation gave nothing", async () => {
    const { deps: d, rec } = deps();
    expect(await storeKeysForPasskeyWith(d, { credentialIdB64: CRED, prfOutput: null })).toBe(
      "stored",
    );
    expect(rec.prompts).toEqual([CRED]);
    expect(rec.enrolled).toHaveLength(1);
  });

  it("is sign-in only when the second prompt gives no PRF output", async () => {
    const { deps: d, rec } = deps({
      evaluatePrf: (id) => Promise.resolve({ credentialIdB64: id, prfOutput: null }),
    });
    expect(await storeKeysForPasskeyWith(d, { credentialIdB64: CRED, prfOutput: null })).toBe(
      "no-prf",
    );
    expect(rec.enrolled).toEqual([]);
    expect(addedPasskeyMessage("no-prf").key).toBe("settings.passkeys.added_no_keys");
  });

  it("adds the passkey sign-in only, with no prompt, when this device holds no phrase", async () => {
    const { deps: d, rec } = deps({ readDevicePhrase: () => Promise.resolve(null) });
    expect(await storeKeysForPasskeyWith(d, { credentialIdB64: CRED, prfOutput: null })).toBe(
      "phrase-needed",
    );
    expect(rec.prompts).toEqual([]);
    expect(rec.enrolled).toEqual([]);
    const line = addedPasskeyMessage("phrase-needed");
    expect(line).toEqual({ kind: "ok", key: "settings.passkeys.added", named: true });
  });

  it("treats a stored phrase of another identity as absent", async () => {
    const { deps: d, rec } = deps({ readDevicePhrase: () => Promise.resolve("other phrase") });
    expect(await storeKeysForPasskeyWith(d, { credentialIdB64: CRED, prfOutput: PRF })).toBe(
      "phrase-needed",
    );
    expect(rec.enrolled).toEqual([]);
  });

  it("says the passkey was added, sign-in only, when the second prompt is dismissed", async () => {
    const { deps: d, rec } = deps({
      evaluatePrf: () => Promise.reject(new WebauthnUserCancelledError()),
    });
    expect(await storeKeysForPasskeyWith(d, { credentialIdB64: CRED, prfOutput: null })).toBe(
      "cancelled",
    );
    expect(rec.enrolled).toEqual([]);
    expect(addedPasskeyMessage("cancelled").key).toBe("settings.passkeys.keys_choice.cancelled");
  });

  it("refuses when a different passkey answers the prompt", async () => {
    const { deps: d, rec } = deps({
      evaluatePrf: () => Promise.resolve({ credentialIdB64: "cred-b", prfOutput: PRF }),
    });
    expect(await storeKeysForPasskeyWith(d, { credentialIdB64: CRED, prfOutput: null })).toBe(
      "other-passkey",
    );
    expect(rec.enrolled).toEqual([]);
    expect(addedPasskeyMessage("other-passkey").key).toBe(
      "settings.passkeys.keys_choice.error.other_passkey",
    );
  });

  it("reports a server without a vault key", async () => {
    const off = deps({ enroll: () => Promise.resolve(false) });
    expect(
      await storeKeysForPasskeyWith(off.deps, { credentialIdB64: CRED, prfOutput: PRF }),
    ).toBe("unavailable");
    const thrown = deps({
      enroll: () => Promise.reject(new AuthHttpError(503, "/vault/passkey", "")),
    });
    expect(
      await storeKeysForPasskeyWith(thrown.deps, { credentialIdB64: CRED, prfOutput: PRF }),
    ).toBe("unavailable");
    expect(addedPasskeyMessage("unavailable").key).toBe(
      "settings.passkeys.keys_choice.unavailable",
    );
  });

  it("reports offline for a network failure at the prompt or the server", async () => {
    const atServer = deps({
      enroll: () => Promise.reject(new AuthNetworkError("/vault/passkey", "down")),
    });
    expect(
      await storeKeysForPasskeyWith(atServer.deps, { credentialIdB64: CRED, prfOutput: PRF }),
    ).toBe("offline");
    const atPrompt = deps({
      evaluatePrf: () => Promise.reject(new WebauthnNetworkError("/webauthn", "down")),
    });
    expect(
      await storeKeysForPasskeyWith(atPrompt.deps, { credentialIdB64: CRED, prfOutput: null }),
    ).toBe("offline");
    expect(addedPasskeyMessage("offline").key).toBe("settings.passkeys.added_keys_failed");
    expect(repairMessage("offline")?.key).toBe("settings.passkeys.error.network");
  });

  it("reports anything else as a generic failure", async () => {
    const { deps: d } = deps({ enroll: () => Promise.reject(new Error("boom")) });
    expect(await storeKeysForPasskeyWith(d, { credentialIdB64: CRED, prfOutput: PRF })).toBe(
      "failed",
    );
    expect(addedPasskeyMessage("failed").key).toBe("settings.passkeys.added_keys_failed");
    expect(repairMessage("failed")?.key).toBe("settings.passkeys.error.generic");
  });
});

describe("restoring keys with a passkey from its row", () => {
  it("uses the typed phrase when it is this account's", async () => {
    const { deps: d, rec } = deps({ readDevicePhrase: () => Promise.resolve(null) });
    expect(
      await storeKeysForPasskeyWith(d, {
        credentialIdB64: CRED,
        prfOutput: null,
        typedPhrase: "  own phrase ",
      }),
    ).toBe("stored");
    expect(rec.enrolled).toEqual([{ prfOutput: PRF, phrase: "own phrase" }]);
    expect(repairMessage("stored")?.key).toBe("settings.passkeys.keys_choice.done");
  });

  it("refuses a typed phrase of another account before any prompt", async () => {
    const { deps: d, rec } = deps({ readDevicePhrase: () => Promise.resolve(null) });
    expect(
      await storeKeysForPasskeyWith(d, {
        credentialIdB64: CRED,
        prfOutput: null,
        typedPhrase: "other phrase",
      }),
    ).toBe("phrase-mismatch");
    expect(rec.prompts).toEqual([]);
    expect(repairMessage("phrase-mismatch")?.key).toBe(
      "settings.passkeys.keys_choice.error.phrase_mismatch",
    );
  });

  it("refuses typed text that is not a phrase", async () => {
    const { deps: d, rec } = deps();
    expect(
      await storeKeysForPasskeyWith(d, {
        credentialIdB64: CRED,
        prfOutput: null,
        typedPhrase: "nonsense",
      }),
    ).toBe("invalid-phrase");
    expect(rec.prompts).toEqual([]);
  });

  it("opens the phrase input, and shows no line, when the device holds no phrase", () => {
    expect(repairMessage("phrase-needed")).toBeNull();
    expect(repairMessage("cancelled")).toBeNull();
  });

  it("asks to choose this passkey when another one answers the row's prompt", () => {
    expect(repairMessage("other-passkey")?.key).toBe(
      "settings.passkeys.keys.error.other_passkey",
    );
  });

  it("says the passkey cannot carry keys here when it gives no PRF output", () => {
    expect(repairMessage("no-prf")?.key).toBe("settings.passkeys.keys_choice.no_prf");
  });
});
