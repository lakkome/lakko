// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether one account key is the PROVEN successor of another (ADR-0074
 * section 3.5a): the same person, across a key rotation the old key signed.
 *
 * Two sources, either suffices:
 *
 *   - the pin store's proven chain (`address-resolver.ts`): the fail-closed
 *     gate verified a supersession proof under a key this device had pinned
 *     for the account, and recorded the replaced key;
 *   - the supersession proof the account's identity bundle carries, verified
 *     HERE under the old key in hand, authorising exactly the key the bundle
 *     publishes. This covers a person this device never pinned (a search
 *     hit, a stranger's contact request). The proof is the one the DIRECTORY
 *     serves for the account, never one handed over by the party that
 *     benefits; the old key's signature over the new key is what it shows.
 *
 * A key change without such a proof (a reset, a substitution) is never a
 * successor here, and neither is anything when the keys are equal.
 *
 * Used by: the contact request addressed to a stale key (rule 1), people
 * search (a hit under a predecessor key is the same person), and the pending
 * contact requests (one request per person, rule 5).
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { loadWasm } from "../wasm";
import { resolveAccountKey } from "./address-resolver";

/** The decision over what was established. Pure. */
export function isProvenSuccessor(facts: {
  readonly oldKeyB64: string;
  readonly newKeyB64: string;
  /** `oldKeyB64` resolved through the proven-predecessor table. */
  readonly resolvedOldKeyB64: string;
  /** The key the account's identity bundle publishes, when one was fetched. */
  readonly bundleKeyB64: string | null;
  /** The key the bundle's supersession proof authorises when verified under
   *  `oldKeyB64`; `null` when there is no proof or it does not verify. */
  readonly proofAuthorisesKeyB64: string | null;
}): boolean {
  if (facts.oldKeyB64 === facts.newKeyB64) return false;
  if (facts.resolvedOldKeyB64 === facts.newKeyB64) return true;
  return (
    facts.bundleKeyB64 !== null &&
    facts.bundleKeyB64 === facts.newKeyB64 &&
    facts.proofAuthorisesKeyB64 === facts.newKeyB64
  );
}

/** The part of an identity bundle the check reads. */
export interface BundleForSuccession {
  readonly signature_pubkey: string;
  readonly supersession_proof?: string | undefined;
}

/**
 * {@link isProvenSuccessor} over the pin store and a fetched bundle (`null`
 * when none was fetched: then only the pin store's chain answers). Never
 * throws.
 */
export async function provenSuccessorWith(
  oldKey: Uint8Array,
  newKey: Uint8Array,
  bundle: BundleForSuccession | null,
): Promise<boolean> {
  try {
    const oldKeyB64 = base64Encode(oldKey);
    const newKeyB64 = base64Encode(newKey);
    let proofAuthorisesKeyB64: string | null = null;
    const proof = bundle?.supersession_proof;
    if (bundle !== null && proof !== undefined && proof.length > 0 && oldKeyB64 !== newKeyB64) {
      try {
        const wasm = await loadWasm();
        proofAuthorisesKeyB64 = base64Encode(
          wasm.verifySupersession(
            base64Decode(proof),
            oldKey,
            BigInt(Math.floor(Date.now() / 1000)),
          ),
        );
      } catch {
        proofAuthorisesKeyB64 = null;
      }
    }
    return isProvenSuccessor({
      oldKeyB64,
      newKeyB64,
      resolvedOldKeyB64: base64Encode(resolveAccountKey(oldKey)),
      bundleKeyB64: bundle?.signature_pubkey ?? null,
      proofAuthorisesKeyB64,
    });
  } catch {
    return false;
  }
}
