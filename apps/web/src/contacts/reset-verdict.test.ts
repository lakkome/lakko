// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 with rule 4 (ADR-0101 section 10) - the adapter over the
 * core's table (`reset-verdict.ts`), driven through the REAL wasm with real
 * signed announcements and reset seals. Every row of
 * `lakko_contacts::reset_verdict` is reached.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as realWasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => realWasm }));

const {
  resetRepinMarker,
  resetVerdict,
} = await import("./reset-verdict");
import type { AttestCheck, LogCheck, ResetVerdictInput } from "./reset-verdict";
import { identityOf, passwordAnnouncement, passwordResetSeal } from "./reset-seal-fixtures";

function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] ?? 0);
  return btoa(s);
}

const ACCOUNT_ID = new Uint8Array(16).fill(0x0a);
const OLD_SEED = 1;
const NEW_SEED = 2;
const PASSWORD = 0x51;
const SIGNED_AT = 1_700_000_000_000;
const VERIFIED: AttestCheck = { kind: "verified", keyGeneration: 2, signedAtMs: SIGNED_AT };
const OFF: LogCheck = { status: "off", isReset: null };
const RESET_LOGGED: LogCheck = { status: "confirmed", isReset: true };

let oldKey: Uint8Array;
let newKey: Uint8Array;
let announcement: Uint8Array;
let seal: Uint8Array;

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
  oldKey = identityOf(realWasm, OLD_SEED).signaturePublicKey;
  newKey = identityOf(realWasm, NEW_SEED).signaturePublicKey;
  announcement = passwordAnnouncement(realWasm, {
    accountId: ACCOUNT_ID,
    signerSeed: OLD_SEED,
    keyGeneration: 1n,
    seq: 7n,
    exportKeyByte: PASSWORD,
  });
  seal = passwordResetSeal(realWasm, {
    accountId: ACCOUNT_ID,
    announcementSeq: 7n,
    fromGeneration: 1n,
    newSignaturePubkey: newKey,
    exportKeyByte: PASSWORD,
  });
});

/** A sealed reset 1 -> 2 with the log off, unless overridden. */
function input(over: Partial<ResetVerdictInput> = {}): ResetVerdictInput {
  return {
    accountId: ACCOUNT_ID,
    heldKeyB64: b64(oldKey),
    acceptedGeneration: 1,
    fetchedKeyB64: b64(newKey),
    bundleGeneration: 2,
    attest: VERIFIED,
    log: OFF,
    previousAnnouncement: announcement,
    resetSeal: seal,
    seqFloor: 0n,
    sealLog: "not-proven",
    ...over,
  };
}

const SEALED = { kind: "sealed", keyGeneration: 2, signedAtMs: SIGNED_AT, announcementSeq: 7n };
const UNSEALED = { kind: "unsealed", keyGeneration: 2, signedAtMs: SIGNED_AT };

describe("resetVerdict: the rows that end before the seal is read", () => {
  it("row 0: an unchanged key is nothing to judge", async () => {
    expect(await resetVerdict(input({ fetchedKeyB64: b64(oldKey) }))).toEqual({ kind: "no-verdict" });
  });

  it("row 1: no attestation is refused, whatever the log and the seal say", async () => {
    for (const log of [
      OFF,
      RESET_LOGGED,
      { status: "unreachable", isReset: null },
      { status: "absent", isReset: null },
    ] as LogCheck[]) {
      expect(await resetVerdict(input({ attest: { kind: "absent" }, log }))).toEqual({
        kind: "refused",
        why: "unconfirmed",
      });
    }
    expect(await resetVerdict(input({ attest: { kind: "invalid" } }))).toEqual({
      kind: "refused",
      why: "unconfirmed",
    });
  });

  it("row 1: no attestation and a log that shows NO reset is `unexplained`", async () => {
    expect(
      await resetVerdict(input({ attest: { kind: "absent" }, log: { status: "confirmed", isReset: false } })),
    ).toEqual({ kind: "refused", why: "unexplained" });
  });

  it("row 2: no attest key is no verdict, not a refusal", async () => {
    expect(await resetVerdict(input({ attest: { kind: "no-key" } }))).toEqual({ kind: "no-verdict" });
  });

  it("row 3: the attested generation must equal the bundle's and exceed the accepted one", async () => {
    expect((await resetVerdict(input({ bundleGeneration: 3 }))).kind).toBe("refused");
    expect((await resetVerdict(input({ acceptedGeneration: 2 }))).kind).toBe("refused");
    expect((await resetVerdict(input({ acceptedGeneration: 5 }))).kind).toBe("refused");
    // A bundle generation that is not a count can never equal the attested one.
    expect((await resetVerdict(input({ bundleGeneration: Number.NaN }))).kind).toBe("refused");
  });

  it("row 4: the log confirms the key WITHOUT a reset: refused, `unexplained`", async () => {
    expect(await resetVerdict(input({ log: { status: "confirmed", isReset: false } }))).toEqual({
      kind: "refused",
      why: "unexplained",
    });
    expect(await resetVerdict(input({ log: { status: "confirmed", isReset: null } }))).toEqual({
      kind: "refused",
      why: "unconfirmed",
    });
  });

  it("row 5: a log mismatch or rollback is refused", async () => {
    for (const status of ["mismatch", "rollback"] as const) {
      expect(await resetVerdict(input({ log: { status, isReset: null } }))).toEqual({
        kind: "refused",
        why: "unconfirmed",
      });
    }
  });

  it("row 6: an unreachable log or an absent entry is no verdict", async () => {
    for (const status of ["unreachable", "absent"] as const) {
      expect(await resetVerdict(input({ log: { status, isReset: null } }))).toEqual({ kind: "no-verdict" });
    }
  });

  it("a key that is not 32 bytes is refused, never judged", async () => {
    expect(await resetVerdict(input({ heldKeyB64: "b2xk" }))).toEqual({ kind: "refused", why: "unconfirmed" });
    expect(await resetVerdict(input({ fetchedKeyB64: "!!" }))).toEqual({ kind: "refused", why: "unconfirmed" });
  });
});

describe("resetVerdict: rule 4, the seal", () => {
  it("row 11: log off, a valid seal: SEALED, with the announcement's seq", async () => {
    expect(await resetVerdict(input())).toEqual(SEALED);
  });

  it("row 10: the log records the reset and proves the announcement: SEALED", async () => {
    expect(await resetVerdict(input({ log: RESET_LOGGED, sealLog: "proven" }))).toEqual(SEALED);
  });

  it("row 8: a valid seal whose announcement the log does not show: UNSEALED", async () => {
    expect(await resetVerdict(input({ log: RESET_LOGGED, sealLog: "not-proven" }))).toEqual(UNSEALED);
  });

  it("row 9: a valid seal and a log that cannot be read for it: no verdict", async () => {
    expect(await resetVerdict(input({ log: RESET_LOGGED, sealLog: "unreachable" }))).toEqual({
      kind: "no-verdict",
    });
  });

  it("row 7: no seal or no announcement is UNSEALED, log off or reset logged", async () => {
    for (const log of [OFF, RESET_LOGGED]) {
      expect(await resetVerdict(input({ log, resetSeal: null, sealLog: "proven" }))).toEqual(UNSEALED);
      expect(await resetVerdict(input({ log, previousAnnouncement: null, sealLog: "proven" }))).toEqual(UNSEALED);
    }
  });

  it("row 7: a seal over ANOTHER new key is UNSEALED", async () => {
    const other = passwordResetSeal(realWasm, {
      accountId: ACCOUNT_ID,
      announcementSeq: 7n,
      fromGeneration: 1n,
      newSignaturePubkey: identityOf(realWasm, 9).signaturePublicKey,
      exportKeyByte: PASSWORD,
    });
    expect(await resetVerdict(input({ resetSeal: other }))).toEqual(UNSEALED);
  });

  it("row 7: a seal by a key the announcement does not list (another password) is UNSEALED", async () => {
    const other = passwordResetSeal(realWasm, {
      accountId: ACCOUNT_ID,
      announcementSeq: 7n,
      fromGeneration: 1n,
      newSignaturePubkey: newKey,
      exportKeyByte: 0x52,
    });
    expect(await resetVerdict(input({ resetSeal: other }))).toEqual(UNSEALED);
  });

  it("row 7: an announcement the HELD key did not sign is UNSEALED", async () => {
    // What an operator that mints a reset can produce: an announcement and a
    // seal under keys of its own, consistent with each other.
    const forged = passwordAnnouncement(realWasm, {
      accountId: ACCOUNT_ID,
      signerSeed: 9,
      keyGeneration: 1n,
      seq: 7n,
      exportKeyByte: PASSWORD,
    });
    expect(await resetVerdict(input({ previousAnnouncement: forged }))).toEqual(UNSEALED);
  });

  it("row 7: an announcement of another account, or of another generation, is UNSEALED", async () => {
    const otherAccount = new Uint8Array(16).fill(0x0b);
    expect(
      await resetVerdict(
        input({
          previousAnnouncement: passwordAnnouncement(realWasm, {
            accountId: otherAccount,
            signerSeed: OLD_SEED,
            keyGeneration: 1n,
            seq: 7n,
            exportKeyByte: PASSWORD,
          }),
        }),
      ),
    ).toEqual(UNSEALED);
    // The device accepted generation 1; an announcement of generation 0 or
    // 2 is not "the previous generation's".
    expect(
      await resetVerdict(
        input({
          previousAnnouncement: passwordAnnouncement(realWasm, {
            accountId: ACCOUNT_ID,
            signerSeed: OLD_SEED,
            keyGeneration: 2n,
            seq: 7n,
            exportKeyByte: PASSWORD,
          }),
        }),
      ),
    ).toEqual(UNSEALED);
  });

  it("row 7: an announcement below this device's seq floor is UNSEALED (a rolled-back one)", async () => {
    expect(await resetVerdict(input({ seqFloor: 8n }))).toEqual(UNSEALED);
    // At the floor is not below it.
    expect(await resetVerdict(input({ seqFloor: 7n }))).toEqual(SEALED);
  });

  it("row 7: bytes that are not an announcement or a seal are UNSEALED, never a throw", async () => {
    expect(await resetVerdict(input({ previousAnnouncement: new Uint8Array(40).fill(1) }))).toEqual(UNSEALED);
    expect(await resetVerdict(input({ resetSeal: new Uint8Array(214).fill(1) }))).toEqual(UNSEALED);
  });

  it("a device that recorded no generation never reads a reset as sealed", async () => {
    // Its announcement is of generation 1, the device holds 0: rule 4 fails.
    expect(await resetVerdict(input({ acceptedGeneration: 0 }))).toEqual(UNSEALED);
  });
});

describe("resetRepinMarker", () => {
  it("never moves the marker backwards", () => {
    expect(resetRepinMarker(0, 5_000)).toBe(5);
    expect(resetRepinMarker(9_999_999_999, 5_000)).toBe(9_999_999_999);
  });
});
