// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The order on a device told `key-generation-superseded` (ADR-0074 section
 * 3.3, `supersededStep`): exchange the refresh credential with a statement by
 * the held key, then by each pending envelope's key; the key the server
 * accepts is the account's. With nothing accepted, read the DeviceGroup
 * journal to its head ONCE and try what it brought. Only then is it a reset.
 *
 * Written over ports so the order is tested; `AuthContext` supplies them.
 */
import type { JournalCatchUp } from "./sync/key-rotation-port";
import { journalReadVerdict, supersededStep, type PendingRotation } from "./key-rotation";

/** What one key's session upgrade came to. */
export type TryKeyOutcome =
  /** The server did not accept this key (or a pending seed could not be
   *  read): the next one. */
  | "not-accepted"
  /** This key is the account's; the device adopted it. */
  | "adopted"
  | "device-revoked"
  | "sign-in"
  /** No usable answer: ask again later. */
  | "later";

export type SupersededOutcome = "adopted" | "reset" | "later" | "device-revoked" | "sign-in";

export interface SupersededPorts {
  /** The key this device holds (standard base64). */
  readonly heldKeyB64: string;
  /** The pending envelopes as stored now (expired ones left out). */
  readonly pending: () => Promise<readonly PendingRotation[]>;
  /** The session upgrade with `keyB64`'s statement, and the adoption when
   *  it is accepted. */
  readonly tryKey: (keyB64: string, pending: readonly PendingRotation[]) => Promise<TryKeyOutcome>;
  /** Read the DeviceGroup journal to its head once; a `key-rotation`
   *  envelope it holds is stored as pending before this resolves. */
  readonly readJournal: () => Promise<JournalCatchUp>;
  /** Whether a session of this account runs in the page (a handle that is
   *  still loading then is a loading order, not a verdict). */
  readonly inSession: () => boolean;
}

export async function settleSuperseded(ports: SupersededPorts): Promise<SupersededOutcome> {
  const tried: string[] = [];
  let journal: "not-read" | "read" | "unreadable" = "not-read";
  for (;;) {
    const pending = await ports.pending();
    const step = supersededStep({ heldKeyB64: ports.heldKeyB64, pending, tried, journal });
    if (step.kind === "reset") return "reset";
    if (step.kind === "read-journal") {
      const read = await ports.readJournal();
      const verdict = journalReadVerdict(read, ports.inSession());
      if (verdict.kind !== "journal") return verdict.kind;
      journal = verdict.journal;
      continue;
    }
    for (const keyB64 of step.keysB64) {
      const outcome = await ports.tryKey(keyB64, pending);
      if (outcome === "not-accepted") {
        tried.push(keyB64);
        continue;
      }
      return outcome;
    }
  }
}
