// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 — the session-wide view of {@link PersonStatusStore}, read by every
 * surface that draws a person (the display resolver, the avatar, the chat
 * screen, person info) and written by the runtime that receives statements
 * and directory answers (`PersonStatusRuntime.tsx`).
 *
 * A module singleton rather than a context: the avatar and the display
 * resolver sit under every provider, and the same state must answer them all.
 * Reactive through one Solid signal bumped on any change; the rules come from
 * the wasm core ({@link wasmSync}), so a reader re-runs once it loaded.
 */

import { createSignal } from "solid-js";

import { wasmSync } from "../wasm";
import {
  directoryCheckIsDue,
  isDeletedPerson,
  MEMORY_PERSON_STATUS_BACKEND,
  PersonStatusStore,
  usernameShowable,
  withAnswer,
  withAttempt,
  withRemoval,
  withStatement,
  type AccountStatusRules,
  type DirectoryAnswerName,
  type PersonStatusBackend,
  type PersonStatusRecord,
} from "./account-status";

const [version, setVersion] = createSignal(0);
const bump = (): void => {
  setVersion((v) => v + 1);
};

let store = new PersonStatusStore(MEMORY_PERSON_STATUS_BACKEND);
let unsubscribe = store.subscribe(bump);

/** The rules port, or `null` while the core is not loaded. Reactive. */
function rules(): AccountStatusRules | null {
  try {
    return wasmSync() as unknown as AccountStatusRules;
  } catch {
    return null;
  }
}

/** Point the singleton at `backend` (the runtime does, once). */
export function usePersonStatusBackend(backend: PersonStatusBackend): void {
  unsubscribe();
  store = new PersonStatusStore(backend, (err) => {
    // eslint-disable-next-line no-console
    console.warn("[person-status] store persist failed", err instanceof Error ? err.name : "unknown");
  });
  unsubscribe = store.subscribe(bump);
}

/** Bind to `accountId` (load its row), or unbind with `null`. */
export async function bindPersonStatus(accountId: string | null): Promise<void> {
  if (accountId === store.boundAccount()) return;
  if (accountId === null) {
    store.close();
    return;
  }
  await store.open(accountId);
}

/** Bumped on any change. */
export const personStatusVersion = version;

/** The record kept for a person (base64 account key). Reactive. */
export function personStatusOf(keyB64: string): PersonStatusRecord | undefined {
  version();
  return store.get(keyB64);
}

/** Every record, `(keyB64, record)`. Reactive. */
export function personStatusEntries(): [string, PersonStatusRecord][] {
  version();
  return [...store.entries()];
}

/** The person is shown as "Deleted account". Reactive. */
export function isDeletedAccount(keyB64: string | null | undefined): boolean {
  if (keyB64 === null || keyB64 === undefined || keyB64.length === 0) return false;
  version();
  const rec = store.get(keyB64);
  if (rec === undefined) return false;
  const r = rules();
  if (r === null) return rec.statementVerified;
  try {
    return isDeletedPerson(r, rec);
  } catch {
    return rec.statementVerified;
  }
}

/** A `@username` may be drawn beside this person now (ADR-0098 section 4);
 *  the caller still requires the certificate to verify. Reactive. */
export function usernameMayShow(keyB64: string, nowMs: number = Date.now()): boolean {
  version();
  const r = rules();
  if (r === null) return false;
  try {
    return usernameShowable(r, store.get(keyB64), nowMs);
  } catch {
    return false;
  }
}

/** The directory may be asked about this person now. Not reactive. */
export function directoryCheckDueFor(keyB64: string, nowMs: number = Date.now()): boolean {
  const r = rules();
  if (r === null) return false;
  try {
    return directoryCheckIsDue(r, store.get(keyB64), nowMs);
  } catch {
    return false;
  }
}

/** A verified statement arrived for this person. */
export function recordDeletionStatement(keyB64: string, accountIdHex?: string): Promise<void> {
  return store.update(keyB64, (rec) => withStatement(rec, accountIdHex));
}

/** A directory answer arrived for this person. */
export function recordDirectoryAnswer(
  keyB64: string,
  answer: DirectoryAnswerName,
  nowMs: number = Date.now(),
  accountIdHex?: string,
): Promise<void> {
  return store.update(keyB64, (rec) => withAnswer(rec, answer, nowMs, accountIdHex));
}

/** This device is about to ask the directory about this person. */
export function recordDirectoryAttempt(keyB64: string, nowMs: number = Date.now()): Promise<void> {
  return store.update(keyB64, (rec) => withAttempt(rec, nowMs));
}

/** ADR-0098 section 5 bookkeeping. */
export function recordRemoval(keyB64: string, groupIdB64: string, state: "owed" | "proposed"): Promise<void> {
  return store.update(keyB64, (rec) => withRemoval(rec, groupIdB64, state));
}

// ---------------------------------------------------------------------------
// Pull trigger
// ---------------------------------------------------------------------------

let checker: ((keyB64: string) => void) | null = null;

/** Install (or remove) what runs a directory check; the runtime owns it. */
export function installPersonChecker(next: ((keyB64: string) => void) | null): void {
  checker = next;
}

/**
 * A person is on screen: ask the directory about them when the rule says it
 * is due. Cheap and safe to call on every render of a row; the checker gates
 * on `directoryCheckDue`, keeps one request in flight per person and asks
 * nothing about a person whose account id it does not know.
 */
export function personOnScreen(keyB64: string | null | undefined): void {
  if (keyB64 === null || keyB64 === undefined || keyB64.length === 0) return;
  try {
    checker?.(keyB64);
  } catch {
    // A check is best-effort.
  }
}

// The people a list row shows right now, counted per row: a row in view adds
// one, a row leaving view or unmounting takes it back. Read by
// `recheckVisiblePeople` only.
const visibleCounts = new Map<string, number>();

function enterView(keyB64: string): void {
  visibleCounts.set(keyB64, (visibleCounts.get(keyB64) ?? 0) + 1);
}

function leaveView(keyB64: string): void {
  const n = visibleCounts.get(keyB64) ?? 0;
  if (n <= 1) visibleCounts.delete(keyB64);
  else visibleCounts.set(keyB64, n - 1);
}

/** The people whose rows are in view now (tests and the runtime). */
export function visiblePeople(): string[] {
  return [...visibleCounts.keys()];
}

/**
 * Ask again about every person whose row is in view. The runtime calls it
 * when this device learns an account id (a group member's profile v2
 * arriving after their row was drawn): a row in view does not intersect
 * again, so without this the member would be asked about only after the list
 * was reopened. Each call goes through the same checker, so the rule, the
 * one-in-flight limit and the account-id requirement still hold.
 */
export function recheckVisiblePeople(): void {
  for (const key of [...visibleCounts.keys()]) personOnScreen(key);
}

/**
 * Ask about the person behind `el` once it is in view: the visible rows of a
 * list, not all of them. Without an `IntersectionObserver` (tests) the row
 * counts as in view at once. Returns the cleanup.
 */
export function checkWhenVisible(el: Element, keyB64: () => string | null | undefined): () => void {
  let shown: string | null = null;
  const show = (): void => {
    if (shown !== null) return;
    const key = keyB64();
    if (key === null || key === undefined || key.length === 0) return;
    shown = key;
    enterView(key);
    personOnScreen(key);
  };
  const hide = (): void => {
    if (shown === null) return;
    leaveView(shown);
    shown = null;
  };
  if (typeof IntersectionObserver === "undefined") {
    show();
    return hide;
  }
  const observer = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (e.isIntersecting) show();
      else hide();
    }
  });
  observer.observe(el);
  return () => {
    observer.disconnect();
    hide();
  };
}
