// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 10, "The account's own devices": a device signed out by
 * a key reset made on another device judges that reset with the same rule a
 * contact does (`contacts/reset-judge.ts`, the core's table), holding its
 * own old key.
 *
 * - `sealed`: the reset was made under a seal the account announced before
 *   it, so by a holder of the account's password, a passkey that keeps the
 *   chats or the phrase: the owner. No "Was it you?": the device is wiped as
 *   for its own reset (ADR-0073 landing rule).
 * - `not-sealed`: unsealed or refused. "Was it you?" and the recall stay.
 * - `unknown`: nothing could be read (offline, the directory down, no
 *   attest key). "Was it you?" stays and asks again at its next check.
 *
 * Never throws.
 */
import { base64Encode } from "./account-id";
import type { IdentityFetchResponse } from "../discovery/wire";
import type { JudgedReset } from "../contacts/reset-judge";

export type OwnResetJudgement = "sealed" | "not-sealed" | "unknown";

export interface OwnResetInput {
  readonly accountId: Uint8Array;
  /** This device's (old) account key, 32 bytes. */
  readonly heldKey: Uint8Array;
  /** The key generation this device's session was of; `null` when not known
   *  (a record read back after a reload): then one below the published
   *  generation, which a single reset made elsewhere is. */
  readonly heldGeneration: number | null;
  /** The highest announcement `seq` this device saw for its own account. */
  readonly seqFloor: bigint;
}

export interface OwnResetPorts {
  /** `GET /discovery/identity/{id}`; `null` when there is no row. Throws on
   *  no answer. */
  readonly fetchBundle: (accountId: Uint8Array) => Promise<IdentityFetchResponse | null>;
  readonly judge: (input: {
    readonly accountId: Uint8Array;
    readonly bundle: IdentityFetchResponse;
    readonly heldKeyB64: string;
    readonly acceptedGeneration: number;
    readonly seqFloor: bigint;
  }) => Promise<JudgedReset>;
}

export async function judgeOwnReset(input: OwnResetInput, ports: OwnResetPorts): Promise<OwnResetJudgement> {
  let bundle: IdentityFetchResponse | null;
  try {
    bundle = await ports.fetchBundle(input.accountId);
  } catch {
    return "unknown";
  }
  if (bundle === null) return "unknown";
  const heldKeyB64 = base64Encode(input.heldKey);
  // The directory still names this device's key: no reset to judge here.
  if (bundle.signature_pubkey === heldKeyB64) return "unknown";
  const accepted = input.heldGeneration ?? Math.max(0, bundle.key_generation - 1);
  try {
    const { verdict } = await ports.judge({
      accountId: input.accountId,
      bundle,
      heldKeyB64,
      acceptedGeneration: accepted,
      seqFloor: input.seqFloor,
    });
    switch (verdict.kind) {
      case "sealed":
        return "sealed";
      case "unsealed":
      case "refused":
        return "not-sealed";
      default:
        return "unknown";
    }
  } catch {
    return "unknown";
  }
}
