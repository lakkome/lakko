// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 (security review 2026-11-21) - which destructive steps a device
 * takes now, from the core's `deletionEffects` per person. "Deleted account"
 * is shown on a verified statement or an `absent` answer; the person's
 * leaves are removed from groups, and the messages held for them dropped,
 * only on a verified statement AND an `absent` answer received after it.
 * Pure: the effects come in through `effectsOf`.
 */

import type { DeletionEffectsView } from "./account-status";

/** The people whose leaves this device should now try to remove. */
export function peopleToRemoveFromGroups(
  personKeysB64: Iterable<string>,
  effectsOf: (keyB64: string) => DeletionEffectsView,
): string[] {
  const out: string[] = [];
  for (const k of personKeysB64) if (effectsOf(k).removeFromGroups) out.push(k);
  return out;
}

/** The pending contacts whose held messages are dropped now (each once). */
export function pendingHeldToDrop<P extends { readonly accountIdHex: string; readonly signaturePubkeyB64: string }>(
  pending: readonly P[],
  effectsOf: (keyB64: string) => DeletionEffectsView,
  alreadyDropped: ReadonlySet<string>,
): P[] {
  return pending.filter((p) => !alreadyDropped.has(p.accountIdHex) && effectsOf(p.signaturePubkeyB64).dropHeldSends);
}
