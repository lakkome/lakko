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
 * ## What this module lists now (ADR-0050 section 2.5a point 4)
 *
 * The add-only detector that used to live here (an own `add` observation
 * that left the pending set without reaching the roster) was the S7 residual's
 * first answer: report the loss, because nothing would repair it. Since
 * section 2.5a a plain member may sweep device admissions, its commit drops
 * every other pending proposal, and **the author re-requests it** —
 * `chat/membership-retry.ts` keeps a durable record of every own request and
 * re-derives it after each merged commit that did not carry it. So a
 * superseded request is no longer a loss and is no longer listed.
 *
 * What is listed is a request whose re-request **stopped** for a reason other
 * than "it landed" or "its subject left": this device no longer holds the
 * authority it needs (the core's typed refusal says `NotPermitted` or
 * `DoNotRetry`), its own membership ended, or an approved join's retained
 * request is gone. The core still reports **no reason** for an exclusion, so
 * no copy here claims one; every kind of request is now covered, because the
 * record names its subject by **key**, never by leaf index, and that was the
 * only reason the old detector was limited to `add`.
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
import type { OwnRequest } from "./membership-retry";

/**
 * What a `Not applied` row is about.
 *
 * ADR-0050 section 2.5a point 4 widened this from `add` only to every own
 * membership request, because a superseded request is now **re-requested**
 * rather than listed (`chat/membership-retry.ts`, `runSupersededRerequests`).
 * A row is written only when that re-request STOPS for a reason other than
 * "the effect landed" or "the subject left the group": this device no longer
 * holds the authority the request needs (a typed refusal whose disposition is
 * `NotPermitted` or `DoNotRetry`), its own membership ended, or — for an
 * approved join — the retained request it would re-approve from is gone.
 *
 * The earlier add-only detector this replaced (an own `add` observation that
 * left the pending set without reaching the roster) is gone: that exact
 * situation is the one the re-request now repairs, so listing it would show
 * as failed a request that is about to be sent again.
 */
export type UnappliedKind =
  | "add"
  | "approve-join"
  | "remove"
  | "leave"
  | "make-admin"
  | "remove-admin"
  | "transfer-ownership"
  | "admit-device"
  | "retire-device";

const UNAPPLIED_KINDS: ReadonlySet<string> = new Set<UnappliedKind>([
  "add",
  "approve-join",
  "remove",
  "leave",
  "make-admin",
  "remove-admin",
  "transfer-ownership",
  "admit-device",
  "retire-device",
]);

/** One own request that will not apply and is no longer being re-requested. */
export interface UnappliedRequest {
  /** `${kind}:${subjectKeyB64}` — the store key and the dismiss handle. */
  readonly key: string;
  readonly kind: UnappliedKind;
  /** base64 of the 32-byte key that names the subject: the person's account
   *  (or legacy leaf) key, the joiner's leaf key for an approved join, the
   *  device's leaf key for a device row. Never a leaf index. */
  readonly subjectKeyB64: string;
}

/** The row for an own request whose re-request stopped. */
export function unappliedFromRequest(request: OwnRequest): UnappliedRequest {
  let kind: UnappliedKind;
  switch (request.kind) {
    case "set-admin":
      kind = request.makeAdmin ? "make-admin" : "remove-admin";
      break;
    default:
      kind = request.kind;
  }
  return { key: `${kind}:${request.subjectKeyB64}`, kind, subjectKeyB64: request.subjectKeyB64 };
}

/**
 * The line under one row.
 *
 * States that it did not apply, **never why**, and offers no "you can do it
 * again": a row exists because the re-request stopped, which is usually
 * because this device no longer holds the authority the request needs, so
 * the offer would be false as often as true. The one exception keeps its
 * action because it is directed at somebody else: an approved join is
 * terminal on the admin side (the core consumed the retained request and only
 * the joiner holds the key material), so the action is theirs.
 *
 * The strings are catalog keys (ADR-0061), one per kind, shared by all three
 * clients.
 */
export function unappliedLine(item: UnappliedRequest): string {
  switch (item.kind) {
    case "add":
      return t("chat.members.unapplied.add_line");
    case "approve-join":
      return t("chat.members.unapplied.join_line");
    case "remove":
      return t("chat.members.unapplied.remove_line");
    case "leave":
      return t("chat.members.unapplied.leave_line");
    case "make-admin":
      return t("chat.members.unapplied.make_admin_line");
    case "remove-admin":
      return t("chat.members.unapplied.remove_admin_line");
    case "transfer-ownership":
      return t("chat.members.unapplied.transfer_line");
    case "admit-device":
      return t("chat.members.unapplied.admit_device_line");
    case "retire-device":
      return t("chat.members.unapplied.retire_device_line");
  }
}

/** The one explanatory line under the section heading. True of every row: a
 *  listed request is one whose automatic re-request has stopped, so nothing
 *  will send it again on its own. */
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

/** A stored row; a dismissed one is remembered so a later stop of the same
 *  request does not resurrect it. */
interface StoredRow {
  readonly kind: UnappliedKind;
  readonly subjectKeyB64: string;
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
      if (
        typeof v?.kind === "string" &&
        UNAPPLIED_KINDS.has(v.kind) &&
        typeof v.subjectKeyB64 === "string" &&
        k === `${v.kind}:${v.subjectKeyB64}`
      ) {
        out[k] = {
          kind: v.kind,
          subjectKeyB64: v.subjectKeyB64,
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

/** The rows still worth showing, keyed on {@link UnappliedRequest.key}.
 *
 *  Sorted by key so the same rows always render in the same order. */
export function loadUnapplied(
  accountIdB64: string,
  groupIdB64: string,
): Map<string, UnappliedRequest> {
  const out = new Map<string, UnappliedRequest>();
  const rows = Object.entries(readRaw(accountIdB64, groupIdB64)).sort(([a], [b]) =>
    a < b ? -1 : a > b ? 1 : 0,
  );
  for (const [k, v] of rows) {
    if (v.dismissed === true) continue;
    out.set(k, { key: k, kind: v.kind, subjectKeyB64: v.subjectKeyB64 });
  }
  return out;
}

/**
 * Record rows. Idempotent, and a **dismissed row is never resurrected** — the
 * same request can stop again after a later re-request, and without the
 * tombstone the notice could not be dismissed for good.
 */
export function recordUnapplied(
  accountIdB64: string,
  groupIdB64: string,
  items: readonly UnappliedRequest[],
): void {
  if (items.length === 0) return;
  const rows = readRaw(accountIdB64, groupIdB64);
  let changed = false;
  for (const item of items) {
    if (rows[item.key] !== undefined) continue;
    rows[item.key] = { kind: item.kind, subjectKeyB64: item.subjectKeyB64 };
    changed = true;
  }
  if (changed) writeRaw(accountIdB64, groupIdB64, rows);
}

/** Dismiss one row by its key. It stays dismissed. */
export function dismissUnapplied(
  accountIdB64: string,
  groupIdB64: string,
  key: string,
): void {
  const rows = readRaw(accountIdB64, groupIdB64);
  const existing = rows[key];
  if (existing === undefined) return;
  rows[key] = { ...existing, dismissed: true };
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
