// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * F3 of the ADR-0098 security pass: every auth call that records the
 * account's identity key (`POST /auth/genesis/attest`, `/account/keys/reset/
 * finish`, `/account/keys/rotate/finish`, `/account/recall/finish`) carries
 * `identity_proof_b64`: an Ed25519 signature by the key being recorded over
 * `"lakko/auth/identity-proof/v1\0" ‖ account_id ‖ pubkey`
 * (`lakko_attest::sign_identity_proof`), so an account can record only a key
 * it holds. Auth answers `400 identity-proof-invalid` for a proof that does
 * not verify and `409 identity-key-taken` for a key another account ever
 * recorded; both take the call's ordinary failure path.
 */

import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { base64UrlEncodeNoPad } from "./account-id";

/** The two core functions this module asks (the wasm surface,
 *  structurally). */
export interface IdentityProofWasm {
  readonly Identity: { fromSeed(seed: Uint8Array): WasmIdentity };
  signIdentityProof(accountIdentity: WasmIdentity, accountId: Uint8Array): Uint8Array;
}

/** The proof's length (an Ed25519 signature). */
export const IDENTITY_PROOF_LEN = 64;

/** The body field the four calls carry. */
export const IDENTITY_PROOF_FIELD = "identity_proof_b64";

/** The proof by `identity` for `accountId` (16 bytes), URL-safe base64
 *  without padding. Throws when the core refuses the account id. */
export function identityProofB64(wasm: IdentityProofWasm, identity: WasmIdentity, accountId: Uint8Array): string {
  const proof = wasm.signIdentityProof(identity, accountId);
  if (proof.length !== IDENTITY_PROOF_LEN) throw new Error("identity proof has the wrong length");
  return base64UrlEncodeNoPad(proof);
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/**
 * The proof by the account identity derived from `seed` (the 64-byte BIP39
 * seed of the phrase the key comes from), refusing when that identity's key
 * is not `expectedPubkey`: the proof must be by the very key the call
 * records.
 */
export function identityProofOfSeedB64(
  wasm: IdentityProofWasm,
  seed: Uint8Array,
  accountId: Uint8Array,
  expectedPubkey: Uint8Array,
): string {
  const identity = wasm.Identity.fromSeed(seed);
  try {
    if (!sameBytes(identity.signaturePublicKey, expectedPubkey)) {
      throw new Error("identity proof: the seed's key is not the key being recorded");
    }
    return identityProofB64(wasm, identity, accountId);
  } finally {
    identity.free();
  }
}
