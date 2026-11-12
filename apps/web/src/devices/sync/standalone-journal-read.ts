// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A superseded device's journal read when no session runs in this page
 * (ADR-0074 section 3.3: "A device whose refresh is answered
 * `key-generation-superseded` and that holds no pending envelope first reads
 * its DeviceGroup journal to the head, once, before it decides anything (the
 * DS does not use session tokens, so it still can)").
 *
 * The page's DeviceGroup handle exists only inside a session, and a device
 * whose refresh was refused on a page load (or that sits on the new-phrase
 * screen of its own reset) has none. Such a device opens its OWN persisted
 * DeviceGroup state into a throwaway handle, reads the journal from the
 * persisted cursor to the head under the access key of the handle's current
 * epoch, applies each row to the throwaway handle, and hands every service
 * envelope it finds to the key-rotation intake. Nothing is persisted: the
 * handle is dropped afterwards, and the session that lands later reads the
 * same rows again from the same cursor.
 *
 * Android's `readDeviceGroupJournalOnce` waits up to 40 s for its LIVE sync
 * runtime to read the head (`awaitHeadOnce`); it does not read on its own.
 * This reads on its own, bounded by the same 40 s.
 *
 * The orchestration is here, free of wasm, DS and IndexedDB, so its rules are
 * tested; `standalone-journal.ts` builds the real handle.
 */
import type { JournalCatchUp } from "./key-rotation-port";

/** How long the read may take in all (Android's `JOURNAL_HEAD_WAIT_MS`). */
export const STANDALONE_JOURNAL_DEADLINE_MS = 40_000;

/** One journal row. */
export interface StandaloneJournalRow {
  readonly id: number;
  readonly payload: Uint8Array;
}

/** This device's DeviceGroup, opened for one read. */
export interface StandaloneJournal {
  /** Where this device's persisted state left the journal. */
  readonly cursor: number;
  /** The rows after `after`, signed under the handle's CURRENT epoch. Throws
   *  what the DS client threw. */
  fetch(after: number): Promise<{ readonly rows: readonly StandaloneJournalRow[]; readonly hasMore: boolean }>;
  /** Apply one row to the throwaway handle; the service envelope it carried,
   *  or `null`. May throw (an own row, a stale row): the read goes on. */
  apply(row: StandaloneJournalRow): Promise<Uint8Array | null>;
  /** Drop the handle. */
  free(): void;
}

export interface StandaloneJournalDeps {
  /** This device's DeviceGroup, or `null` when the device holds no usable
   *  DeviceGroup state at all. */
  readonly open: () => Promise<StandaloneJournal | null>;
  /** The key-rotation intake: store a verified `key-rotation` envelope as
   *  pending; anything else is ignored. */
  readonly intake: (envelope: Uint8Array) => Promise<void>;
  /** What a failed fetch means: the journal refuses this device
   *  (`unreadable`), or there was no answer (`offline`). */
  readonly classify: (err: unknown) => "unreadable" | "offline";
  readonly deadlineMs?: number;
  readonly now?: () => number;
}

/**
 * Read to the head once. `not-loaded` only when the device holds no
 * DeviceGroup state (a device that never joined one, or one that dropped an
 * unusable snapshot); `read` once the DS said there is nothing more;
 * `unreadable` when the journal refuses this device; `offline` on no answer
 * or when the deadline passed first.
 */
export async function readJournalStandalone(deps: StandaloneJournalDeps): Promise<JournalCatchUp> {
  const now = deps.now ?? Date.now;
  const deadline = now() + (deps.deadlineMs ?? STANDALONE_JOURNAL_DEADLINE_MS);
  let journal: StandaloneJournal | null;
  try {
    journal = await deps.open();
  } catch {
    return "offline";
  }
  if (journal === null) return "not-loaded";
  try {
    let cursor = journal.cursor;
    while (now() < deadline) {
      let page: { readonly rows: readonly StandaloneJournalRow[]; readonly hasMore: boolean };
      try {
        page = await journal.fetch(cursor);
      } catch (err) {
        return deps.classify(err);
      }
      for (const row of page.rows) {
        if (row.id > cursor) cursor = row.id;
        let envelope: Uint8Array | null = null;
        try {
          envelope = await journal.apply(row);
        } catch {
          envelope = null;
        }
        if (envelope === null) continue;
        try {
          await deps.intake(envelope);
        } catch {
          // A row the intake could not store says nothing about the others.
        }
      }
      if (!page.hasMore || page.rows.length === 0) return "read";
    }
    return "offline";
  } finally {
    try {
      journal.free();
    } catch {
      /* already freed */
    }
  }
}
