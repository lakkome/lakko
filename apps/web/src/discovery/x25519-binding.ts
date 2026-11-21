// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account X25519 key a device may seal to (security review 2026-11-21).
 *
 * The directory serves an account's Ed25519 signature key and its X25519
 * key side by side. Without a binding, a directory that controls the row can
 * pair a person's real signature key with an X25519 key of its own and read
 * everything sealed "to that person". Every upload therefore carries
 * `x25519_binding` (64 bytes, `signX25519Binding(accountIdentity)`), and every
 * fetch hands it back.
 *
 * The one rule, used at every site that seals to a fetched `x25519_pubkey`:
 * the key is used only when `verifyX25519Binding(binding, signature_pubkey,
 * x25519_pubkey)` is true. An absent, malformed or failing binding means the
 * key is not used at all: the flow ends in the failure it already has, and
 * nothing is sealed. There is no fallback.
 */

import { loadWasm } from "../wasm";
import type { IdentityFetchResponse } from "./wire";

/** The fields of a fetched bundle the rule reads. */
export type BindingFields = Pick<
  IdentityFetchResponse,
  "signature_pubkey" | "x25519_pubkey" | "x25519_binding"
>;

/** The verifier, `verifyX25519Binding` of the wasm core. */
export type X25519BindingVerifier = (
  binding: Uint8Array,
  ed25519PublicKey: Uint8Array,
  x25519PublicKey: Uint8Array,
) => boolean;

/** Thrown where a flow needs the key and the binding does not verify. */
export class X25519BindingRefused extends Error {
  override name = "X25519BindingRefused";
  constructor() {
    super("the recipient's published keys could not be confirmed");
  }
}

function decodeStd(b64: string): Uint8Array | null {
  try {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/**
 * The bundle's X25519 key when its binding verifies under its signature key,
 * else `null`. Pure: the verifier is passed in.
 */
export function boundX25519With(
  verify: X25519BindingVerifier,
  bundle: BindingFields,
): Uint8Array | null {
  const bindingB64 = bundle.x25519_binding;
  if (bindingB64 === undefined || bindingB64 === null || bindingB64.length === 0) return null;
  const binding = decodeStd(bindingB64);
  const signature = decodeStd(bundle.signature_pubkey);
  const x25519 = decodeStd(bundle.x25519_pubkey);
  if (binding === null || signature === null || x25519 === null) return null;
  if (signature.length !== 32 || x25519.length !== 32) return null;
  let ok = false;
  try {
    ok = verify(binding, signature, x25519);
  } catch {
    ok = false;
  }
  return ok ? x25519 : null;
}

/** {@link boundX25519With} with the wasm core's verifier. */
export async function boundX25519(bundle: BindingFields): Promise<Uint8Array | null> {
  const wasm = await loadWasm();
  return boundX25519With((b, e, x) => wasm.verifyX25519Binding(b, e, x), bundle);
}

/** {@link boundX25519}, throwing {@link X25519BindingRefused} on `null`. */
export async function requireBoundX25519(bundle: BindingFields): Promise<Uint8Array> {
  const key = await boundX25519(bundle);
  if (key === null) throw new X25519BindingRefused();
  return key;
}
