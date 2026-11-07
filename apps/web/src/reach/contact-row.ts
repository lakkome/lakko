// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 1 (amended 2026-11-06) — what the receiving device does
 * with one mailbox row, by the key that admitted it. Pure orchestration over
 * the core's one rule (`mailboxRowContactVerdict`):
 *
 * - no admitting key named (a proofless row), or one that is not a retired
 *   `contact` key: process as before;
 * - a retired `contact` key from a current contact: process, and hand them the
 *   current grant (republish the profile in the one-to-one), nothing shown;
 * - a retired `contact` key from anyone else: drop, nothing shown, nothing
 *   logged that names anyone.
 */

export type ContactRowAction = "process" | "regrant" | "drop";

export interface ContactRowInput {
  /** The admitting reach key the DS returned for the row, if any. */
  readonly reachPk: Uint8Array | undefined;
  /** The core's rule: the verdict for this key and sender standing. */
  readonly verdict: (reachPk: Uint8Array, senderIsContact: boolean) => ContactRowAction;
  /** Whether the row's verified sender is a current contact; asked only for
   *  a retired `contact` key. */
  readonly senderIsContact: () => boolean;
}

export function contactRowAction(input: ContactRowInput): ContactRowAction {
  if (input.reachPk === undefined) return "process";
  // A key that is not a retired contact key is processed whoever sent it.
  if (input.verdict(input.reachPk, false) === "process") return "process";
  return input.verdict(input.reachPk, input.senderIsContact());
}

/** Whether `keyB64` (or a key it aliases) names a current contact: in the
 *  list and not removed. */
export function isCurrentContactKey(
  keyB64: string,
  findContact: (keyB64: string) => { readonly signaturePubkeyB64: string } | undefined,
  isRemoved: (keyB64: string) => boolean,
): boolean {
  const c = findContact(keyB64);
  return c !== undefined && !isRemoved(c.signaturePubkeyB64);
}
