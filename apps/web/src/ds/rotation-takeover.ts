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
