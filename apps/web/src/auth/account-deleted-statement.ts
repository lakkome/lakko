// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 1 — the send side of the account-deleted statement.
 *
 * After `DELETE /account` succeeded at auth and before this device is wiped,
 * one statement is posted as an ordinary MLS application message into every
 * conversation this device holds (one-to-ones and groups; not channels, not
 * the DeviceGroup). Each post is one more best-effort cleanup of
 * {@link runAccountDeletion}: inside the same budget, a failure logged by its
 * fixed label only, never stopping the wipe. Never before the auth deletion:
 * auth may refuse, and then the statement would be false.
 *
 * The Delivery Service needs no session for it: a group post is proved by the
 * group's epoch key (ADR-0053), which still works once the session is gone.
 *
 * The signing key is the account key this device's conversation leaves are
 * bound under (after an ADR-0074 rotation, the conversation key), the very
 * key every receiver resolves the sender to (C-68).
 *
 * Kept free of wasm, IndexedDB and network imports so it runs under vitest;
 * the caller hands the composer and the publisher in.
 */

import type { DeletionCleanup } from "./account-deletion";

/** The fixed log label of one statement post. Never an identifier. */
export const ACCOUNT_DELETED_CLEANUP_LABEL = "account-deleted-statement";

/** One conversation the statement goes to. */
export interface StatementTarget {
  readonly groupId: Uint8Array;
  /** Seal `plaintext` at the group's live epoch and publish it. */
  readonly publish: (plaintext: Uint8Array) => Promise<void>;
}

/**
 * The conversations a statement goes to: a one-to-one or a group (the core
 * names a conversation kind), still active. A PublisherGroup or a DeviceGroup
 * names no kind and is skipped.
 */
export function isStatementConversation(group: {
  groupKind(): number | undefined;
  readonly isActive: boolean;
}): boolean {
  try {
    if (group.groupKind() === undefined) return false;
    return group.isActive !== false;
  } catch {
    return false;
  }
}

/**
 * One cleanup per target. `compose` is the core's
 * `composeAccountDeletedEnvelope(seed, groupId)`; it runs inside the cleanup,
 * so a refusal is logged by label like any other failure.
 */
export function buildStatementCleanups(
  targets: readonly StatementTarget[],
  signingSeed: Uint8Array,
  compose: (seed: Uint8Array, groupId: Uint8Array) => Uint8Array,
): DeletionCleanup[] {
  if (signingSeed.length !== 32) return [];
  return targets.map((target) => ({
    label: ACCOUNT_DELETED_CLEANUP_LABEL,
    run: async () => {
      const envelope = compose(signingSeed, target.groupId);
      await target.publish(envelope);
    },
  }));
}

/**
 * The identity whose key the conversation leaves are bound under, among the
 * candidates this device holds (the conversation identity, the account
 * identity, the own predecessors): the one whose signature key equals the
 * device's `boundKey`. `null` when none matches (then nothing is sent: a
 * statement signed by any other key would be dropped by every receiver).
 */
export function bindingSigner<I extends { readonly signaturePublicKey: Uint8Array }>(
  boundKey: Uint8Array | undefined,
  candidates: readonly (I | null | undefined)[],
): I | null {
  if (boundKey === undefined || boundKey.length !== 32) return null;
  for (const c of candidates) {
    if (c === null || c === undefined) continue;
    const k = c.signaturePublicKey;
    if (k.length === boundKey.length && k.every((b, i) => b === boundKey[i])) return c;
  }
  return null;
}
