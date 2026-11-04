// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.5 ("DS") - after a key rotation, every journal this
 * account roots moves from the root derived from the OLD seed to the root
 * derived from the new one. For a conversation group the account owns this is
 * exactly an ownership handover to oneself: the old root signs a
 * `RootTransfer` naming the new root, then the new root replaces the
 * registrar set and re-sends the registrar seeds. `JournalOwnershipProvider`
 * already does that for a handover (`takeOverJournal`, with its own persisted
 * "pending root seed" and its retries); this module is the one door the
 * rotation uses to start it for every owned group.
 */

/** Start the takeover of every conversation journal this account owns, with
 *  the root seeds derived from `oldAccountEd25519Seed`. Returns how many were
 *  started; each then finishes (and retries) by itself. */
export type RotationTakeOver = (oldAccountEd25519Seed: Uint8Array) => Promise<number>;

let installed: RotationTakeOver | null = null;

export function installRotationTakeOver(run: RotationTakeOver): () => void {
  installed = run;
  return () => {
    if (installed === run) installed = null;
  };
}

/** `null` when the provider is not mounted (no session). */
export async function takeOverOwnedJournals(oldAccountEd25519Seed: Uint8Array): Promise<number | null> {
  return installed === null ? null : installed(oldAccountEd25519Seed);
}

/**
 * ADR-0087 section 5 - the knock keys of the groups this account owns derive
 * from the journal root, which the rotation moved. One owed step bumps each
 * group's knock version to the new root, records the boundary (earlier
 * versions stay with the predecessor's root), records the public record's
 * republish, and hands the new seed to the admins at the next reconcile.
 * Returns how many groups moved, or throws to keep the step owed.
 */
export type RotationKnockKeys = (oldAccountKeyB64: string) => Promise<number>;

let knockKeys: RotationKnockKeys | null = null;

export function installRotationKnockKeys(run: RotationKnockKeys): () => void {
  knockKeys = run;
  return () => {
    if (knockKeys === run) knockKeys = null;
  };
}

/** `null` when the provider is not mounted (no session). */
export async function rotateOwnedKnockKeys(oldAccountKeyB64: string): Promise<number | null> {
  return knockKeys === null ? null : knockKeys(oldAccountKeyB64);
}

/**
 * ADR-0087 sections 3 and 5 - the handle key of a group derives from the
 * journal root too, so each `@name` an owned group holds is re-keyed (the
 * `rekey` statement, signed by the old key) and its in-group envelope sent
 * again. The step records one owed rekey per name, durably; the handle
 * runtime runs them. Returns how many were recorded, or throws.
 */
export type RotationHandleRekeys = (oldAccountKeyB64: string) => Promise<number>;

let handleRekeys: RotationHandleRekeys | null = null;

export function installRotationHandleRekeys(run: RotationHandleRekeys): () => void {
  handleRekeys = run;
  return () => {
    if (handleRekeys === run) handleRekeys = null;
  };
}

/** `null` when the runtime is not mounted (no session). */
export async function recordOwnedHandleRekeys(oldAccountKeyB64: string): Promise<number | null> {
  return handleRekeys === null ? null : handleRekeys(oldAccountKeyB64);
}
