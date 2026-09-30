// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 S7 residual 1 — the request that was not carried.
 *
 * ## The gap
 *
 * A sweep does not necessarily carry everything it finds. `sweep_keeps`
 * enumerates fifteen `CommitBuilder::build` conditions and
 * `narrow_sweep_by_exclusion` drops whatever still refuses to build — and the
 * commonest cause needs no adversary at all: a **KeyPackage that expired**,
 * reachable precisely because ADR-0045 decision 2 gives a request no deadline.
 *
 * Before this module, an excluded request was simply *gone*. OpenMLS empties
 * the whole proposal store on every merged commit, so the pending set that was
 * the request's only representation is cleared whether or not the commit
 * carried it; `pruneObservations` then removes the local observation; and the
 * panel falls silent. For an **approved join** it is worse than silent, because
 * a successful `approve_external_join` **consumes** the retained request
 * (`drop_pending_external_join`, once the proposal is on its way), so the
 * quarantine entry is gone too and the admin cannot even re-approve.
 *
 * ## What is actually knowable — and what is not
 *
 * The core reports **no reason**: `CreateCommitError` carries no proposal
 * reference on any variant, so "why" does not exist at this layer and no copy
 * here may claim one. What exists is the **observation**, and this module takes
 * the one that reaches the person who can act:
 *
 * > **A request of mine left the core's pending set without its effect landing.**
 *
 * That test runs on **every** device rather than only on the sweeper — which
 * matters, because the device that swept is often not the device that asked —
 * and it needs no core change, no new wire artifact and no new FFI. It is also
 * *exactly* as strong as the truth: the pending set is empty, so no sweep will
 * ever carry the request again.
 *
 * ## Why only an `add`
 *
 * `remove:` and `leave:` keys name a **leaf index**, and OpenMLS hands a
 * vacated leaf to the next `Add`, so a roster test on a leaf can be wrong in
 * both directions. `role-change` carries no payload at all. And all three are
 * already **visible** when they fail — the member you tried to remove is still
 * in the roster, the group you failed to leave is still in your list. The
 * `add` is the only request whose failure leaves no trace anywhere, and the
 * only one whose effect test is keyed on a **signature key** rather than a
 * leaf. So the scope is not a shortcut: it is the exact set where the test is
 * sound and the silence is real.
 *
 * ## The one accepted false positive
 *
 * A member added by our sweep and removed again before the next render reads as
 * "not added". It needs two commits between two folds and its cost is one
 * dismissible line offering an action that is harmless to take. Narrowing it
 * would need the core to say which commit emptied the set, which is the reason
 * this layer does not have.
 *
 * ## What the JOINER is told: nothing, and that is a finding rather than a gap
 *
 * A link joiner is a **non-member polling the group journal**. There is no
 * artifact addressed to it, and the journal is unauthenticated — the DS
 * pseudonym IS the `group_id` and there is no auth on it, so anyone who has
 * ever held the `/g/` link can post — which makes any "your request was not
 * carried" envelope a **forgeable denial primitive** rather than a signal.
 * Inference does not work either: an epoch advancing with no rendezvous for us
 * is byte-for-byte what *no admin has approved yet* looks like, which is the
 * ordinary state of a request that by design has no deadline. So the joiner
 * gets no notice. What it gets instead is the **repair**: `routes/JoinGroup.tsx`
 * offers `Request again`, user-driven and never automatic, which re-mints a
 * fresh KeyPackage — the one thing that actually fixes the commonest cause.
 */

import { t } from "../l10n";
import type { RequestObservation } from "./pending-requests";

/** One own `add` request that left the pending set without landing. */
export interface UnappliedAdd {
  /** base64 of the prospective member's 32-byte Ed25519 signature key. */
  readonly addSignatureKeyB64: string;
  /**
   * `true` when this device published the request by approving an external
   * **join** rather than by adding a contact.
   *
   * It changes the copy and nothing else, and it is knowable *only* on the
   * device that made the request — which is the same device this notice is
   * shown on. A sweeping third party genuinely cannot tell the two apart: on
   * the wire an approved add is indistinguishable from an admin adding a
   * contact, which is ADR-0045 S7's point rather than a limitation.
   */
  readonly viaJoinRequest: boolean;
}

/**
 * The own `add` requests that are dead: observed as ours, no longer in the
 * core's live pending set, and not in the roster.
 *
 * Pure. `liveKeys` and `rosterKeysB64` must be read in the SAME pass — the
 * pending set is emptied by the merge that also moves the roster, so reading
 * one before and one after a commit would compare two different moments.
 *
 * A `null` roster is a torn read, not an empty group, and claims NOTHING —
 * the same fail-closed direction `membershipRotationDecision` takes with the
 * identical signal. Treating an unreadable roster as "nobody is in it" would
 * report every own add as unapplied at once.
 */
export function unappliedAddRequests(
  observations: ReadonlyMap<string, RequestObservation>,
  liveKeys: readonly string[],
  rosterKeysB64: readonly string[] | null,
): UnappliedAdd[] {
  if (rosterKeysB64 === null) return [];
  const live = new Set(liveKeys);
  const roster = new Set(rosterKeysB64);
  const out: UnappliedAdd[] = [];
  for (const [key, seen] of observations) {
    // UX §10 — a member is never told about someone else's request.
    if (!seen.own) continue;
    if (!key.startsWith("add:")) continue;
    const addKeyB64 = key.slice("add:".length);
    // `proposalKey` writes `add:?` when the record carried no signature key.
    // That names nobody, so there is nothing honest to say about it.
    if (addKeyB64.length === 0 || addKeyB64 === "?") continue;
    if (live.has(key)) continue;
    if (roster.has(addKeyB64)) continue;
    out.push({ addSignatureKeyB64: addKeyB64, viaJoinRequest: seen.joinRequest === true });
  }
  return out;
}

/**
 * The line under one row.
 *
 * States that it did not apply and what can be done, **never why** — the core
 * has no reason to give and inventing one would be the same class of lie as a
 * green E2EE pill over an unencrypted call. The UX spec §7 draft string
 * (`This request expired before it could be applied.`) asserts a cause this
 * client cannot establish; the divergence is deliberate.
 *
 * A join approval is **terminal on the admin side** — the approve consumed the
 * retained request and only the joiner holds the key material — so it directs
 * the action to them. An ordinary add is re-doable here.
 */
export function unappliedLine(item: UnappliedAdd): string {
  return item.viaJoinRequest
    ? t("chat.members.unapplied.join_line")
    : t("chat.members.unapplied.add_line");
}

/** The one explanatory line under the section heading. True on both counts:
 *  the request left the pending set, so no sweep will carry it, and no client
 *  re-requests (UX §7's automatic re-request is designed and not built). */
export const UNAPPLIED_EXPLANATION = (): string => t("chat.members.unapplied.explanation");

/** The section heading. */
export function unappliedHeading(count: number): string {
  return t("chat.members.unapplied.heading", { count });
}

// ---------------------------------------------------------------------------
// The joiner's half of the same residual
// ---------------------------------------------------------------------------

/**
 * What `/g/` says while a request is outstanding.
 *
 * The joiner is told **nothing** about an exclusion, for the reasons in this
 * module's header — there is no honest channel. What it is told instead is the
 * truth about the wait itself, which the previous copy left out: **both** steps
 * are unbounded, and a request can quietly not happen.
 */
export const JOIN_WAIT_PRIMARY = (): string => t("group.join.wait_primary");

/** The second line — the part that was missing, and the only thing that makes
 *  the `Request again` control legible. */
export const JOIN_WAIT_SECONDARY = (): string => t("group.join.wait_secondary");

/** The joiner's one repair, and the ONLY thing available to this side.
 *
 *  A fresh request mints a fresh KeyPackage, which is what fixes the commonest
 *  cause of an exclusion — but the button never says so, because this side
 *  cannot know that was the cause. It is deliberately **user-driven and never
 *  automatic**: each press is a journal row held to the DS's 30-day TTL, and a
 *  timer would spin against a group that is merely quiet.
 *
 *  Its reach is honest but partial, and the limit is real: an admin still
 *  holding an earlier request for the same signature key **keeps the older
 *  KeyPackage** (`retain_external_join` de-dups by signature key and skips the
 *  push), so a re-request refreshes the material only where the earlier entry
 *  is gone — which is exactly the case that matters here, because a successful
 *  approve consumed it. */
export const JOIN_REQUEST_AGAIN = (): string => t("group.join.request_again");

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

const STORE_PREFIX = "lakko.unapplied-requests.v1";

/** A dismissed row, remembered so the detection pass cannot resurrect it. */
interface StoredRow {
  readonly viaJoinRequest: boolean;
  readonly dismissed?: boolean;
}

function storeKey(accountIdB64: string, groupIdB64: string): string {
  return `${STORE_PREFIX}.${accountIdB64}.${groupIdB64}`;
}

function readRaw(accountIdB64: string, groupIdB64: string): Record<string, StoredRow> {
  try {
    const raw = globalThis.localStorage?.getItem(storeKey(accountIdB64, groupIdB64));
    if (raw === null || raw === undefined) return {};
    const parsed = JSON.parse(raw) as Record<string, StoredRow>;
    const out: Record<string, StoredRow> = {};
    for (const [k, v] of Object.entries(parsed)) {
      if (typeof v?.viaJoinRequest === "boolean") {
        out[k] = {
          viaJoinRequest: v.viaJoinRequest,
          ...(v.dismissed === true ? { dismissed: true } : {}),
        };
      }
    }
    return out;
  } catch {
    return {};
  }
}

function writeRaw(
  accountIdB64: string,
  groupIdB64: string,
  rows: Record<string, StoredRow>,
): void {
  try {
    globalThis.localStorage?.setItem(
      storeKey(accountIdB64, groupIdB64),
      JSON.stringify(rows),
    );
  } catch {
    /* the notice is best-effort; nothing here is load-bearing for correctness */
  }
}

/** The rows still worth showing, keyed on the signature key.
 *
 *  Sorted by key, matching the iOS store: `Map` order would otherwise follow
 *  whatever order the JSON happened to carry, and the three clients should not
 *  render the same two rows in two different orders. */
export function loadUnapplied(
  accountIdB64: string,
  groupIdB64: string,
): Map<string, UnappliedAdd> {
  const out = new Map<string, UnappliedAdd>();
  const rows = Object.entries(readRaw(accountIdB64, groupIdB64)).sort(([a], [b]) =>
    a < b ? -1 : a > b ? 1 : 0,
  );
  for (const [k, v] of rows) {
    if (v.dismissed === true) continue;
    out.set(k, { addSignatureKeyB64: k, viaJoinRequest: v.viaJoinRequest });
  }
  return out;
}

/**
 * Record detected rows. Idempotent, and a **dismissed row is never
 * resurrected** — the detection re-runs on every fold, so without the tombstone
 * the notice could not be dismissed at all.
 */
export function recordUnapplied(
  accountIdB64: string,
  groupIdB64: string,
  items: readonly UnappliedAdd[],
): void {
  if (items.length === 0) return;
  const rows = readRaw(accountIdB64, groupIdB64);
  let changed = false;
  for (const item of items) {
    const existing = rows[item.addSignatureKeyB64];
    if (existing !== undefined) continue;
    rows[item.addSignatureKeyB64] = { viaJoinRequest: item.viaJoinRequest };
    changed = true;
  }
  if (changed) writeRaw(accountIdB64, groupIdB64, rows);
}

/** Dismiss one row. It stays dismissed. */
export function dismissUnapplied(
  accountIdB64: string,
  groupIdB64: string,
  addSignatureKeyB64: string,
): void {
  const rows = readRaw(accountIdB64, groupIdB64);
  const existing = rows[addSignatureKeyB64];
  if (existing === undefined) return;
  rows[addSignatureKeyB64] = { viaJoinRequest: existing.viaJoinRequest, dismissed: true };
  writeRaw(accountIdB64, groupIdB64, rows);
}

/** Drop every row for one account — `recover` / `forgetDevice` hygiene, the
 *  same discipline the observation store and the commit outbox follow. */
export function clearUnappliedForAccount(accountIdB64: string): void {
  try {
    const store = globalThis.localStorage;
    if (store === undefined || store === null) return;
    const victims: string[] = [];
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i);
      if (k !== null && k.startsWith(`${STORE_PREFIX}.${accountIdB64}.`)) victims.push(k);
    }
    for (const k of victims) store.removeItem(k);
  } catch {
    /* best-effort */
  }
}

/** Drop every row for one group — `Delete this chat` hygiene. */
export function clearUnappliedForGroup(
  accountIdB64: string,
  groupIdB64: string,
): void {
  try {
    globalThis.localStorage?.removeItem(storeKey(accountIdB64, groupIdB64));
  } catch {
    /* best-effort */
  }
}
