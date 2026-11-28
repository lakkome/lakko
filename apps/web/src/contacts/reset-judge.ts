// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 with rule 4 over one fetched identity bundle: gather the
 * rule's inputs (the attestation under the trusted attest key, the key log,
 * the announcement and the reset seal the directory serves) and ask the
 * core's table (`reset-verdict.ts`). Writes nothing.
 *
 * Two callers: the peer side (`client.ts`, a contact's reset) and the
 * account's own old device (`auth/own-reset-verdict.ts`, ADR-0101 section 10
 * "The account's own devices": a sealed reset made elsewhere is the
 * owner's, an unsealed one is answered with "Was it you?").
 */
import { base64Decode } from "../auth/account-id";
import { fetchDeviceAttestKey } from "../auth/device-attest-key";
import type { IdentityFetchResponse } from "../discovery/wire";
import { verifyIdentityInLog } from "../transparency/verify";
import { loadWasm } from "../wasm";
import { resetVerdict, type AttestCheck, type LogCheck, type ResetVerdict, type SealLog } from "./reset-verdict";

function optionalBytes(b64: string | undefined): Uint8Array | null {
  if (b64 === undefined || b64.length === 0) return null;
  try {
    const bytes = base64Decode(b64);
    return bytes.length > 0 ? bytes : null;
  } catch {
    return null;
  }
}

/** The attestation a bundle carries, verified for this account and the
 *  served key under the trusted attest key. Never throws. */
export async function checkResetAttestation(
  accountId: Uint8Array,
  servedPubkey: Uint8Array,
  bundle: IdentityFetchResponse,
): Promise<AttestCheck> {
  const raw = bundle.key_reset_attest;
  if (raw === undefined || raw.length === 0) return { kind: "absent" };
  let attestation: Uint8Array;
  try {
    attestation = base64Decode(raw);
  } catch {
    return { kind: "invalid" };
  }
  let attestKey: Uint8Array;
  try {
    attestKey = await fetchDeviceAttestKey();
  } catch {
    return { kind: "no-key" };
  }
  try {
    const wasm = await loadWasm();
    const verified = wasm.verifyKeyResetAttestation(attestation, attestKey, accountId, servedPubkey);
    const keyGeneration = Number(verified.keyGeneration);
    const signedAtMs = Number(verified.signedAtMillis);
    try {
      verified.free();
    } catch {
      // Freeing is best-effort.
    }
    return { kind: "verified", keyGeneration, signedAtMs };
  } catch {
    return { kind: "invalid" };
  }
}

/** What the rule answered, and the key log's answer it read. */
export interface JudgedReset {
  readonly verdict: ResetVerdict;
  readonly log: LogCheck;
}

/** The core's verdict on `bundle` for a holder of `heldKeyB64`. Never
 *  throws. */
export async function judgeBundleReset(input: {
  readonly accountId: Uint8Array;
  readonly bundle: IdentityFetchResponse;
  readonly heldKeyB64: string;
  readonly acceptedGeneration: number;
  readonly seqFloor: bigint;
}): Promise<JudgedReset> {
  const { accountId, bundle } = input;
  let servedPubkey: Uint8Array;
  try {
    servedPubkey = base64Decode(bundle.signature_pubkey);
  } catch {
    return { verdict: { kind: "refused", why: "unconfirmed" }, log: { status: "unreachable", isReset: null } };
  }
  const attest = await checkResetAttestation(accountId, servedPubkey, bundle);
  // Rule 4's inputs as the directory served them. Bytes that do not
  // decode read as absent: the reset is then unsealed, never refused.
  const previousAnnouncement = optionalBytes(bundle.previous_reset_seals);
  const resetSeal = optionalBytes(bundle.reset_seal);
  let log: LogCheck;
  let sealLog: SealLog = "not-proven";
  try {
    const r = await verifyIdentityInLog(
      accountId,
      servedPubkey,
      {},
      previousAnnouncement === null ? undefined : { previousAnnouncement },
    );
    log = { status: r.status, isReset: r.isReset };
    if (r.sealLog !== undefined) sealLog = r.sealLog;
  } catch {
    log = { status: "unreachable", isReset: null };
  }
  const verdict = await resetVerdict({
    accountId,
    heldKeyB64: input.heldKeyB64,
    acceptedGeneration: input.acceptedGeneration,
    fetchedKeyB64: bundle.signature_pubkey,
    bundleGeneration: bundle.key_generation,
    attest,
    log,
    previousAnnouncement,
    resetSeal,
    seqFloor: input.seqFloor,
    sealLog,
  });
  return { verdict, log };
}
