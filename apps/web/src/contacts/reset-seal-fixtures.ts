// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * TEST SUPPORT (no production caller): real seal announcements and reset
 * seals (ADR-0101 section 10), built with the wasm core's own signers, for
 * the suites that drive the reset verdict and the identity gate.
 *
 * The caller passes the initialised wasm module, so this file imports no
 * loader and works under any test's `vi.mock("../wasm")`.
 */
import type * as WasmModule from "../../wasm/lakko_ffi_wasm";

type Wasm = typeof WasmModule;

/** A password export key (64 bytes) all of one byte. */
export function exportKeyOf(byte: number): Uint8Array {
  return new Uint8Array(64).fill(byte);
}

/** The account identity the core derives from a 32-byte seed of one byte. */
export function identityOf(wasm: Wasm, seedByte: number): WasmModule.Identity {
  return wasm.Identity.fromSeed(new Uint8Array(32).fill(seedByte));
}

/**
 * A seal announcement of `accountId`, signed by the account identity of
 * `signerSeed`, listing ONE password seal made from `exportKeyByte`.
 */
export function passwordAnnouncement(
  wasm: Wasm,
  input: {
    readonly accountId: Uint8Array;
    readonly signerSeed: number;
    readonly keyGeneration: bigint;
    readonly seq: bigint;
    readonly exportKeyByte: number;
  },
): Uint8Array {
  const sealKey = wasm.ResetSealKey.fromPasswordExportKey(exportKeyOf(input.exportKeyByte));
  try {
    return wasm.signSealAnnouncement(
      identityOf(wasm, input.signerSeed),
      input.accountId,
      input.keyGeneration,
      input.seq,
      1_700_000_000_000n,
      ["password"],
      sealKey.announcementTag(),
      sealKey.publicKey,
    );
  } finally {
    sealKey.free();
  }
}

/**
 * A reset seal from `fromGeneration` to `fromGeneration + 1` over
 * `newSignaturePubkey`, signed by the password seal key of `exportKeyByte`.
 */
export function passwordResetSeal(
  wasm: Wasm,
  input: {
    readonly accountId: Uint8Array;
    readonly announcementSeq: bigint;
    readonly fromGeneration: bigint;
    readonly newSignaturePubkey: Uint8Array;
    readonly exportKeyByte: number;
  },
): Uint8Array {
  const sealKey = wasm.ResetSealKey.fromPasswordExportKey(exportKeyOf(input.exportKeyByte));
  try {
    return sealKey.signResetSeal(
      input.accountId,
      input.announcementSeq,
      input.fromGeneration,
      input.newSignaturePubkey,
    );
  } finally {
    sealKey.free();
  }
}
