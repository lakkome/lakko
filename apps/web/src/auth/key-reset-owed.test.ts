// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The "publish owed" record and the step rules of a key reset
 * (`key-reset-owed.ts`, ADR-0073 section 2.3, UX 15.4): the two server
 * steps run in order with the same attestation, a step that already landed
 * reads as done, the record clears only when both have, a stale attestation
 * waits for an explicit sign-in, and the phrase is withheld while anything
 * is owed.
 */
import { describe, expect, it } from "vitest";

import {
  attestationAge,
  attestationGate,
  clearKeyResetOwed,
  deviceGroupResetOutcome,
  directoryResetOutcome,
  isKeyResetOwed,
  KEY_RESET_ATTESTATION_LEN,
  KEY_RESET_ATTESTATION_MAX_AGE_MS,
  keyResetAttestationGeneration,
  keyResetAttestationSignedAtMs,
  phraseRevealStep,
  readKeyResetOwed,
  remainingKeyResetSteps,
  withKeyResetStepDone,
  writeKeyResetOwed,
  type KeyResetOwed,
} from "./key-reset-owed";
import type { MarkerStorage } from "./phrase-gate";

class MemoryStorage implements MarkerStorage {
  private readonly map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
}

const ACCT = "QUJDRA==";
const NOW = 1_800_000_000_000;

const FRESH: KeyResetOwed = {
  v: 1,
  attestationB64Url: "YXR0ZXN0",
  keyGeneration: "2",
  newSignaturePubkeyB64: "cHVia2V5",
  signedAtMs: NOW,
  done: { directory: false, deviceGroup: false },
};

describe("the record", () => {
  it("round-trips, and reads as owed until both steps are done", () => {
    const storage = new MemoryStorage();
    expect(readKeyResetOwed(storage, ACCT)).toBeNull();
    expect(isKeyResetOwed(storage, ACCT)).toBe(false);
    writeKeyResetOwed(storage, ACCT, FRESH);
    expect(readKeyResetOwed(storage, ACCT)).toEqual(FRESH);
    expect(isKeyResetOwed(storage, ACCT)).toBe(true);
    writeKeyResetOwed(storage, ACCT, withKeyResetStepDone(FRESH, "directory"));
    expect(isKeyResetOwed(storage, ACCT)).toBe(true);
    writeKeyResetOwed(
      storage,
      ACCT,
      withKeyResetStepDone(withKeyResetStepDone(FRESH, "directory"), "device-group"),
    );
    expect(isKeyResetOwed(storage, ACCT)).toBe(false);
    clearKeyResetOwed(storage, ACCT);
    expect(readKeyResetOwed(storage, ACCT)).toBeNull();
  });

  it("is per account", () => {
    const storage = new MemoryStorage();
    writeKeyResetOwed(storage, ACCT, FRESH);
    expect(isKeyResetOwed(storage, "other")).toBe(false);
  });

  it("treats a damaged row as absent rather than driving a retry from it", () => {
    const storage = new MemoryStorage();
    storage.setItem(`lakko:key-reset-owed:${ACCT}`, "{not json");
    expect(readKeyResetOwed(storage, ACCT)).toBeNull();
    storage.setItem(`lakko:key-reset-owed:${ACCT}`, JSON.stringify({ v: 2 }));
    expect(readKeyResetOwed(storage, ACCT)).toBeNull();
    storage.setItem(
      `lakko:key-reset-owed:${ACCT}`,
      JSON.stringify({ ...FRESH, done: { directory: "yes" } }),
    );
    expect(readKeyResetOwed(storage, ACCT)?.done).toEqual({ directory: false, deviceGroup: false });
  });

  it("survives a blocked storage", () => {
    expect(() => writeKeyResetOwed(undefined, ACCT, FRESH)).not.toThrow();
    expect(readKeyResetOwed(undefined, ACCT)).toBeNull();
    expect(isKeyResetOwed(undefined, ACCT)).toBe(false);
  });
});

describe("the step order", () => {
  it("runs the directory before the device group, and only what is not done", () => {
    expect(remainingKeyResetSteps({ directory: false, deviceGroup: false })).toEqual([
      "directory",
      "device-group",
    ]);
    expect(remainingKeyResetSteps({ directory: true, deviceGroup: false })).toEqual(["device-group"]);
    // The device-group step cannot be done before the directory, but a
    // record saying so still runs only what is missing.
    expect(remainingKeyResetSteps({ directory: false, deviceGroup: true })).toEqual(["directory"]);
    expect(remainingKeyResetSteps({ directory: true, deviceGroup: true })).toEqual([]);
  });

  it("marks a step done without touching the other, and never un-marks", () => {
    const one = withKeyResetStepDone(FRESH, "device-group");
    expect(one.done).toEqual({ directory: false, deviceGroup: true });
    expect(withKeyResetStepDone(one, "directory").done).toEqual({ directory: true, deviceGroup: true });
    expect(withKeyResetStepDone(one, "device-group").done).toEqual(one.done);
    expect(one.attestationB64Url).toBe(FRESH.attestationB64Url);
  });
});

describe("the directory step's answer", () => {
  it("a 2xx is done, and so is a replay the server reports as not newer", () => {
    expect(directoryResetOutcome({ status: 200, errorCode: null })).toBe("done");
    expect(directoryResetOutcome({ status: 409, errorCode: "key-generation-not-newer" })).toBe("done");
  });

  it("a superseded session is the signed-out path, never a retry", () => {
    expect(directoryResetOutcome({ status: 401, errorCode: "key-generation-superseded" })).toBe(
      "superseded",
    );
  });

  it("a refused attestation is reported, not looped", () => {
    expect(directoryResetOutcome({ status: 400, errorCode: "key-reset-attestation-invalid" })).toBe(
      "refused",
    );
    expect(
      directoryResetOutcome({ status: 409, errorCode: "identity-change-needs-attestation" }),
    ).toBe("refused");
  });

  it("everything else is a retry", () => {
    expect(directoryResetOutcome({ status: 429, errorCode: "identity-changed-too-recently" })).toBe("retry");
    expect(directoryResetOutcome({ status: 503, errorCode: "key-reset-disabled" })).toBe("retry");
    expect(directoryResetOutcome({ status: 500, errorCode: null })).toBe("retry");
    expect(directoryResetOutcome({ status: 0, errorCode: null })).toBe("retry");
    expect(directoryResetOutcome({ status: 401, errorCode: null })).toBe("retry");
  });
});

describe("the device-group step's answer", () => {
  it("a 2xx is done; a 409 is done only when the root probe confirms this root", () => {
    expect(deviceGroupResetOutcome({ status: 204, errorCode: null, probeConfirmedRoot: null })).toBe("done");
    expect(deviceGroupResetOutcome({ status: 409, errorCode: null, probeConfirmedRoot: true })).toBe("done");
    expect(deviceGroupResetOutcome({ status: 409, errorCode: null, probeConfirmedRoot: false })).toBe("retry");
    expect(deviceGroupResetOutcome({ status: 409, errorCode: null, probeConfirmedRoot: null })).toBe("retry");
  });

  it("a stranger's 401 and a bad attestation are refused; a 503 and the rest retry", () => {
    expect(deviceGroupResetOutcome({ status: 401, errorCode: null, probeConfirmedRoot: null })).toBe("refused");
    expect(
      deviceGroupResetOutcome({ status: 400, errorCode: "key-reset-attestation-invalid", probeConfirmedRoot: null }),
    ).toBe("refused");
    expect(deviceGroupResetOutcome({ status: 503, errorCode: "key-reset-disabled", probeConfirmedRoot: null })).toBe("retry");
    expect(deviceGroupResetOutcome({ status: 0, errorCode: null, probeConfirmedRoot: null })).toBe("retry");
  });
});

describe("the 30-day bound", () => {
  it("is usable up to the bound and too old past it; a clock behind the mint reads as fresh", () => {
    expect(attestationAge(NOW, NOW)).toBe("usable");
    expect(attestationAge(NOW, NOW + KEY_RESET_ATTESTATION_MAX_AGE_MS)).toBe("usable");
    expect(attestationAge(NOW, NOW + KEY_RESET_ATTESTATION_MAX_AGE_MS + 1)).toBe("too-old");
    expect(attestationAge(NOW + 10_000, NOW)).toBe("usable");
  });

  it("applies a fresh attestation always, a stale one only after an explicit sign-in", () => {
    const stale = NOW + KEY_RESET_ATTESTATION_MAX_AGE_MS + 1;
    expect(attestationGate(NOW, NOW, false)).toBe("apply");
    expect(attestationGate(NOW, NOW, true)).toBe("apply");
    expect(attestationGate(NOW, stale, false)).toBe("sign-in-needed");
    expect(attestationGate(NOW, stale, true)).toBe("apply");
  });
});

describe("the attestation's wire fields", () => {
  function blob(generation: bigint, signedAt: bigint): Uint8Array {
    const out = new Uint8Array(KEY_RESET_ATTESTATION_LEN);
    const view = new DataView(out.buffer);
    out[0] = 1;
    view.setBigUint64(1 + 32 + 32, generation, false);
    view.setBigInt64(1 + 32 + 32 + 8 + 32, signedAt, false);
    return out;
  }

  it("reads signed_at and the generation at their fixed offsets", () => {
    const b = blob(3n, BigInt(NOW));
    expect(keyResetAttestationSignedAtMs(b)).toBe(NOW);
    expect(keyResetAttestationGeneration(b)).toBe(3n);
    // A view into a larger buffer reads the same.
    const padded = new Uint8Array(10 + KEY_RESET_ATTESTATION_LEN);
    padded.set(b, 10);
    expect(keyResetAttestationSignedAtMs(padded.subarray(10))).toBe(NOW);
  });

  it("refuses a blob of the wrong length", () => {
    expect(() => keyResetAttestationSignedAtMs(new Uint8Array(176))).toThrow(/177/);
    expect(() => keyResetAttestationGeneration(new Uint8Array(178))).toThrow(/177/);
  });
});

describe("Show recovery phrase", () => {
  it("withholds the words while the publish is owed, shows them otherwise, says so when none is saved", () => {
    expect(phraseRevealStep({ phraseOnDevice: false, publishOwed: false })).toBe("not-saved");
    expect(phraseRevealStep({ phraseOnDevice: false, publishOwed: true })).toBe("not-saved");
    expect(phraseRevealStep({ phraseOnDevice: true, publishOwed: true })).toBe("publish-pending");
    expect(phraseRevealStep({ phraseOnDevice: true, publishOwed: false })).toBe("reveal");
  });
});
