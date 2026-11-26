// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 4 - fetch a reset person's identity bundle and keep the
 * continuity mailbox its binding names, verified under the key this device
 * ACCEPTED for the account (`attestedKeyB64`, the accepted reset's pin). The
 * one fetch the peer side runs before writing to a continuity mailbox (the
 * conversation's successor Welcome, a channel return, a profile). A failed
 * fetch keeps what is held; a bundle naming another key forgets it.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import type { DiscoveryClient } from "../discovery/client";
import { loadWasm } from "../wasm";
import {
  continuityTargetOf,
  peerContinuityTarget,
  rememberContinuityTarget,
  type ContinuityTarget,
} from "./continuity";

export async function refreshContinuityTarget(
  discovery: Pick<DiscoveryClient, "identityFetch">,
  accountIdB64: string,
  attestedKeyB64: string,
  nowMs: number = Date.now(),
): Promise<ContinuityTarget | null> {
  try {
    const bundle = await discovery.identityFetch(base64Decode(accountIdB64));
    if (bundle === null || bundle.signature_pubkey !== attestedKeyB64) {
      rememberContinuityTarget(attestedKeyB64, null, nowMs);
      return null;
    }
    const wasm = await loadWasm();
    const target = peerContinuityTarget(
      bundle.continuity_binding_b64,
      base64Decode(attestedKeyB64),
      nowMs,
      (b, k, now) => wasm.verifyContinuityBinding(b, k, now),
      base64Decode,
      base64Encode,
    );
    rememberContinuityTarget(attestedKeyB64, target, nowMs);
    return target;
  } catch {
    return continuityTargetOf(attestedKeyB64, nowMs);
  }
}
