// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Durable pending-commit outbox (ADR-0040, G-35).
 *
 * ## Why this exists
 *
 * The four group-membership commit paths (add / remove / role-change) each run: core create-commit (which eagerly merges
 * epoch N -> N+1 in the in-memory OpenMLS provider) -> publish the commit (and,
 * for add / approve, a Welcome / rendezvous) to the Delivery Service -> persist
 * the N+1 snapshot best-effort. A crash — or a swallowed persist failure — in the
 * window between publish and persist left the commit DURABLE on the DS (peers
 * advance to N+1) while the local snapshot rolled back to N: a silent,
 * single-device membership brick (the device is stuck at N, cannot re-apply its
 * own commit, and its future N-epoch messages are rejected by peers at N+1).
 *
 * A bare persist-before-publish reorder does NOT fix this — it flips the desync
 * to "device durably at N+1 with the commit never published", stranding the whole
 * group. The commit's *durability*, not its *ordering*, must change: the commit
 * (and its Welcome) has to survive the crash so it can be RE-PUBLISHED to catch
 * the group up. That is what this outbox holds.
 *
 * ## Atomicity — single IndexedDB transaction (ADR-0040 Open Question 1)
 *
 * The outbox lives in the SAME `lakko-mls` IndexedDB database as the MLS snapshot
 * (`mls-store.ts`), in a sibling `commit-outbox` object store. The commit path
 * writes the outbox entry AND the N+1 snapshot in ONE atomic
 * `transaction([snapshots, commit-outbox], "readwrite")`
 * (`saveSnapshotWithCommitEntry` in `mls-store.ts`) BEFORE any publish. IndexedDB
 * multi-store transactions are all-or-nothing, so:
 *
 *   - the forbidden state "snapshot at N+1 with NO outbox entry" (the whole-group
 *     strand) is impossible by construction, and
 *   - an un-acked outbox entry ALWAYS means the device is durably at N+1, so the
 *     restart sweep can re-publish UNCONDITIONALLY with no torn-write / epoch
 *     reconciliation (the entry-first-then-snapshot alternative the ADR also
 *     sanctions would need that reconciliation; the single-tx path does not).
 *
 * ## What an entry holds (invariants #1 / #2 preserved)
 *
 * MLS commit ciphertext + a HPKE-sealed Welcome / rendezvous — opaque PUBLIC
 * artefacts the DS already stores verbatim. No plaintext, no PII, nothing that
 * does not already leave the device, so the entry is stored as plain base64 (it
 * is not the security-critical snapshot, which stays device-key-encrypted in the
 * sibling store).
 *
 * ## Retention
 *
 * An acked entry is dropped on the next sweep (or explicitly). An un-acked entry
 * that keeps failing to re-publish (a persistently unreachable DS) is left for the
 * next sweep — a retry/backoff + "membership change pending" UX is a deferred
 * follow-up (ADR-0040 Open questions).
 */

import { requireAccountStoresWritable } from "../auth/account-store-gate";
import { OUTBOX_STORE, awaitTx, openMlsDb } from "./mls-store";
import { boundContactsAccount } from "../contacts/local-store";

/**
 * The re-publish destination of a DIRECT-path entry's second artifact.
 *
 * One member since ADR-0045 S7. The external-join approve used to write a
 * `"rendezvous"` entry riding the group journal; it is a proposal now, so it
 * commits nothing, records nothing, and the sweep owns every rendezvous
 * (`chat/sweep.ts`).
 */
export type SecondArtifactKind = "welcome";

/**
 * The accept of a contact request whose group stands and whose Welcome or ack
 * has not landed (`contacts/owed-answer.ts`). The entry carries no commit: the
 * founding commit fanned out to nobody. Its Welcome is `secondArtifactB64`.
 * `sweepVerdictPending` and `sweepWelcomesOwed` are both set, so the restart
 * loop leaves it alone and a new grant finds it (`chat/owed-welcomes.ts`); the
 * sweep's own fan-out passes it over, since it would use other keys.
 */
export interface OwedAnswer {
  /** The `reply` grant the request carried. Secret. */
  readonly replyGrantB64: string | null;
  /** The request's fields the ack is built from. */
  readonly fromPubkeyB64: string;
  readonly fromPseudonymB64: string;
  /** Who the Welcome goes to, own mailbox first. */
  readonly targets: readonly { readonly accountKeyB64: string; readonly isRecipient: boolean }[];
  /** The Welcome landed; only the ack is owed. */
  readonly welcomeLanded?: boolean;
}

/**
 * One durable pending-commit record. The primary key is `id` (a random hex
 * string). The commit + Welcome / rendezvous bytes are stored base64 — opaque
 * MLS public artefacts (ADR-0040; invariants #1 / #2).
 */
export interface PendingCommitEntry {
  /** Random 16-byte hex id — the IDB primary key. */
  id: string;
  /** base64(account_id) — set by the persist hook so a logout / account switch
   *  can purge stale entries and the sweep never crosses accounts. */
  accountId: string;
  /** base64(group_id) — the sweep filters entries on this. */
  groupId: string;
  /** Decimal string of the post-commit epoch (N+1). Diagnostics / GC only: the
   *  single-atomic-tx write makes an un-acked entry unconditionally mean
   *  "durably at N+1", so the sweep needs no epoch reconciliation. */
  targetEpoch: string;
  /** base64 of the EXACT commit wire-bytes the core create-commit produced. */
  commitB64: string;
  /** base64 of the second artifact: the RAW MLS Welcome. ADR-0078: a
   *  mailbox row is sealed to the mailbox key the write resolves, so the row
   *  is built at (re)publish, not here. Absent for remove / role (they
   *  publish only the commit). */
  secondArtifactB64?: string;
  /** Discriminates the second artifact's re-publish destination. */
  secondArtifactKind?: SecondArtifactKind;
  /** ADR-0060 — base64 of the ACCOUNT key the second artifact goes to: a
   *  re-publish finds a grant held for that account, verifies its mailbox
   *  binding against this key (ADR-0078), seals to the key it names and
   *  signs with the grant. */
  secondArtifactAccountKeyB64?: string;
  /**
   * ADR-0045 — this entry belongs to a STAGED sweep, so its commit is on the
   * wire but the local epoch has NOT advanced and a journal verdict is still
   * owed.
   *
   * The flag is what keeps the restart sweep from treating the two shapes
   * alike, and the difference is not cosmetic: for a legacy direct-commit entry
   * the device is durably at N+1 and the second artifact may be re-published
   * unconditionally, whereas for a sweep entry the Welcome must NOT be
   * published until the verdict says the commit won (ADR-0045's Welcome
   * ordering rule — publishing it on a commit that then loses puts a joiner
   * into a state no other member holds).
   *
   * Absent on every entry the four direct paths write.
   */
  sweepVerdictPending?: boolean;
  /**
   * ADR-0045 — this sweep's verdict is SETTLED (its commit won and merged) and
   * the only thing it still owes is the Welcome fan-out.
   *
   * Set in one durable write at the moment of the merge, which is what keeps
   * obligation 2 intact: the flag IS the durable statement "the verdict said
   * WON", so an entry that has not reached a verdict never carries it and a
   * pre-verdict restart therefore cannot be talked into publishing a Welcome.
   *
   * `sweepVerdictPending` stays set alongside it, deliberately — that flag is
   * what marks the entry as a SWEEP entry, and the ADR-0040 legacy re-publish
   * loop skips on it. Clearing it would hand a welcome-owed entry to a loop
   * that re-publishes the commit, finds no singular
   * `secondArtifactAccountKeyB64`, publishes no Welcome at all, and then ACKS —
   * destroying the record. The two flags are read together:
   * `sweepVerdictPending && !sweepWelcomesOwed` is verdict-owed,
   * `sweepWelcomesOwed` is fan-out-owed.
   */
  sweepWelcomesOwed?: boolean;
  /** Set on the entry of an accepted contact request. See {@link OwedAnswer}. */
  answer?: OwedAnswer;
  /**
   * ADR-0045 — the sweep's Welcome fan-out targets: base64 mailbox pseudonyms,
   * one per member the sweep adds.
   *
   * A list, where the direct add path has the singular
   * `secondArtifactAccountKeyB64`, because one sweep commit can carry several adds
   * under a single Welcome. Empty when the sweep carried no add.
   */
  welcomeMailboxesB64?: string[];
  /** `false` until BOTH artifacts have (re)published. An un-acked entry is swept
   *  on restart / group-open. */
  acked: boolean;
  /** DS ids once known — recorded on ack for echo-filter diagnostics. */
  commitMessageId?: number;
  secondMessageId?: number;
}

/**
 * The subset a commit path supplies. The persist hook (`ChatContext`) fills the
 * account scope + the `acked: false` flag, so the caller never has to know the
 * account id or the durability bookkeeping.
 */
export type NewCommitEntry = Omit<
  PendingCommitEntry,
  "accountId" | "acked" | "commitMessageId" | "secondMessageId"
>;

/**
 * Persist a single outbox entry durably, awaited, and THROW on failure —
 * fail-closed. This is the outbox-store-only write (used by tests + as a building
 * block); the production commit path co-writes the entry AND the snapshot in one
 * atomic transaction via `saveSnapshotWithCommitEntry` in `mls-store.ts`.
 */
export async function putEntryDurable(entry: PendingCommitEntry): Promise<void> {
  const db = await openMlsDb();
  try {
    requireAccountStoresWritable(entry.accountId);
    const tx = db.transaction(OUTBOX_STORE, "readwrite");
    tx.objectStore(OUTBOX_STORE).put(entry);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * Load every un-acked entry of `owner` (default: the signed-in account bound
 * through `bindContactsAccount`), optionally filtered to one group's base64
 * id. The store is tiny (one entry per in-flight commit), so a `getAll` +
 * in-memory filter is simplest and robust.
 *
 * The store is shared by every account on this browser (each entry carries
 * its `accountId`), so the read filters on the account: an entry is that
 * account's MLS state, and two accounts that are members of one group must
 * never re-publish, adjudicate or drop each other's commits (2026-09-30 —
 * the read used to return every account's entries). With no account the
 * result is empty.
 */
export async function loadUnacked(
  groupIdB64?: string,
  owner?: string,
): Promise<PendingCommitEntry[]> {
  const account = owner ?? boundContactsAccount();
  if (account === null || account.length === 0) return [];
  const db = await openMlsDb();
  try {
    const all = await new Promise<PendingCommitEntry[]>((resolve, reject) => {
      const tx = db.transaction(OUTBOX_STORE, "readonly");
      const req = tx.objectStore(OUTBOX_STORE).getAll();
      req.onsuccess = (): void =>
        resolve((req.result as PendingCommitEntry[] | undefined) ?? []);
      req.onerror = (): void =>
        reject(req.error ?? new Error("commit-outbox getAll failed"));
    });
    return all.filter(
      (e) =>
        e.acked === false &&
        e.accountId === account &&
        (groupIdB64 === undefined || e.groupId === groupIdB64),
    );
  } finally {
    db.close();
  }
}

/**
 * Mark an entry acked and record the published DS ids. Read-modify-write inside
 * one transaction so the other fields survive. A no-op when the entry is already
 * gone (a concurrent sweep / delete raced us).
 */
export async function markAcked(
  id: string,
  commitMessageId?: number,
  secondMessageId?: number,
): Promise<void> {
  const db = await openMlsDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(OUTBOX_STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onabort = (): void =>
        reject(tx.error ?? new Error("commit-outbox markAcked aborted"));
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("commit-outbox markAcked errored"));
      const store = tx.objectStore(OUTBOX_STORE);
      const getReq = store.get(id);
      getReq.onsuccess = (): void => {
        const row = getReq.result as PendingCommitEntry | undefined;
        if (row === undefined) return; // already gone — tx completes as a no-op
        // Build without assigning `undefined` (exactOptionalPropertyTypes).
        const updated: PendingCommitEntry = { ...row, acked: true };
        if (commitMessageId !== undefined) {
          updated.commitMessageId = commitMessageId;
        }
        if (secondMessageId !== undefined) {
          updated.secondMessageId = secondMessageId;
        }
        store.put(updated);
      };
    });
  } finally {
    db.close();
  }
}

/**
 * ADR-0045 — flip a sweep entry from *verdict owed* to *Welcome fan-out owed*,
 * recording the DS id its commit published under, in ONE read-modify-write
 * transaction so it is never half-applied.
 *
 * THROWS on failure. The caller treats that as non-fatal: the marker only buys
 * a retry, and the Welcome it guards is correct to publish either way.
 *
 * A no-op when the entry is already gone (a concurrent ack / discard raced us).
 */
export async function markWelcomesOwed(
  id: string,
  commitMessageId: number,
): Promise<void> {
  const db = await openMlsDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(OUTBOX_STORE, "readwrite");
      tx.oncomplete = (): void => resolve();
      tx.onabort = (): void =>
        reject(tx.error ?? new Error("commit-outbox markWelcomesOwed aborted"));
      tx.onerror = (): void =>
        reject(tx.error ?? new Error("commit-outbox markWelcomesOwed errored"));
      const store = tx.objectStore(OUTBOX_STORE);
      const getReq = store.get(id);
      getReq.onsuccess = (): void => {
        const row = getReq.result as PendingCommitEntry | undefined;
        if (row === undefined) return; // already gone — tx completes as a no-op
        store.put({ ...row, sweepWelcomesOwed: true, commitMessageId });
      };
    });
  } finally {
    db.close();
  }
}

/** Delete an entry by id. No-op when absent. */
export async function deleteEntry(id: string): Promise<void> {
  const db = await openMlsDb();
  try {
    const tx = db.transaction(OUTBOX_STORE, "readwrite");
    tx.objectStore(OUTBOX_STORE).delete(id);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * Drop every entry of one group (ADR-0087 section 9): whatever it still owed
 * (a re-publish, a Welcome fan-out) is moot once the group is gone from this
 * device. No-op when there is nothing to drop.
 */
export async function deleteOutboxForGroup(
  accountIdB64: string,
  groupIdB64: string,
): Promise<void> {
  const db = await openMlsDb();
  try {
    const rows = await new Promise<PendingCommitEntry[]>((resolve, reject) => {
      const tx = db.transaction(OUTBOX_STORE, "readonly");
      const req = tx.objectStore(OUTBOX_STORE).getAll();
      req.onsuccess = (): void =>
        resolve((req.result as PendingCommitEntry[] | undefined) ?? []);
      req.onerror = (): void =>
        reject(req.error ?? new Error("commit-outbox getAll failed"));
    });
    const victims = rows.filter((r) => r.accountId === accountIdB64 && r.groupId === groupIdB64);
    if (victims.length === 0) return;
    const tx = db.transaction(OUTBOX_STORE, "readwrite");
    const store = tx.objectStore(OUTBOX_STORE);
    for (const v of victims) store.delete(v.id);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}

/**
 * Purge every entry for an account — logout / recover / forget-device hygiene, so
 * a prior session's pending commits never get swept into a later one. No-op when
 * there is nothing to purge.
 */
export async function deleteOutboxForAccount(
  accountIdB64: string,
): Promise<void> {
  const db = await openMlsDb();
  try {
    const rows = await new Promise<PendingCommitEntry[]>((resolve, reject) => {
      const tx = db.transaction(OUTBOX_STORE, "readonly");
      const req = tx.objectStore(OUTBOX_STORE).getAll();
      req.onsuccess = (): void =>
        resolve((req.result as PendingCommitEntry[] | undefined) ?? []);
      req.onerror = (): void =>
        reject(req.error ?? new Error("commit-outbox getAll failed"));
    });
    const victims = rows.filter((r) => r.accountId === accountIdB64);
    if (victims.length === 0) return;
    const tx = db.transaction(OUTBOX_STORE, "readwrite");
    const store = tx.objectStore(OUTBOX_STORE);
    for (const v of victims) store.delete(v.id);
    await awaitTx(tx, undefined);
  } finally {
    db.close();
  }
}
