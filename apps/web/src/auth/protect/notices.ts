// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the account's own devices are told (ADR-0075 section 9,
 * `docs/ux/identity-recovery.md` 19.2, 19.5 and 19.6): which of the notices
 * `GET /account/notices` returned are shown on this device, in what words,
 * and the two per-device facts behind that:
 *
 *   - the seen cursor: the highest notice id this device has dealt with;
 *   - the "I did this" markers: a device is not told about its own action,
 *     so each action it performs is remembered by kind and time, and a
 *     notice of that kind written within {@link SELF_WINDOW_MS} of it is
 *     skipped.
 *
 * A notice carries a kind and a time and nothing else. Pure, with the stores
 * in plain storage.
 */
import type { MarkerStorage } from "../phrase-gate";

export const NOTICE_KINDS = [
  "password-changed-by-email",
  "account-opened-by-email",
  "recovery-email-added",
  "recovery-email-replaced",
  "recovery-email-removed",
  "recovery-phrase-saved",
] as const;
export type NoticeKind = (typeof NOTICE_KINDS)[number];

export interface AccountNotice {
  readonly id: number;
  readonly kind: NoticeKind;
  readonly atMs: number;
}

/** One page of `GET /account/notices`. */
export interface NoticePage {
  readonly notices: readonly AccountNotice[];
  readonly nextAfter: number;
  readonly more: boolean;
}

function isKind(value: unknown): value is NoticeKind {
  return typeof value === "string" && (NOTICE_KINDS as readonly string[]).includes(value);
}

function nonNegativeInt(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

/**
 * Read a `GET /account/notices` body. A notice of a kind this build does not
 * know is dropped (a newer server); its id still moves the cursor through
 * `next_after`. `null` when the body is not a page.
 */
export function parseNoticePage(body: unknown): NoticePage | null {
  if (typeof body !== "object" || body === null) return null;
  const b = body as Record<string, unknown>;
  const nextAfter = nonNegativeInt(b["next_after"]);
  if (!Array.isArray(b["notices"]) || nextAfter === null || typeof b["more"] !== "boolean") return null;
  const notices: AccountNotice[] = [];
  for (const raw of b["notices"] as unknown[]) {
    if (typeof raw !== "object" || raw === null) continue;
    const n = raw as Record<string, unknown>;
    const id = nonNegativeInt(n["id"]);
    const atMs = typeof n["at_ms"] === "number" && Number.isFinite(n["at_ms"]) ? n["at_ms"] : null;
    if (id === null || atMs === null || !isKind(n["kind"])) continue;
    notices.push({ id, kind: n["kind"], atMs });
  }
  return { notices, nextAfter, more: b["more"] };
}

// ---------------------------------------------------------------------------
// Which notices this device shows
// ---------------------------------------------------------------------------

/** A notice written this close to this device's own action of the same kind
 *  is that action. Chosen: wide enough for a slow request and a clock that
 *  differs from the server's by a few minutes. */
export const SELF_WINDOW_MS = 10 * 60 * 1000;

/** This device performed an action of `kind` at `atMs` (its own clock). */
export interface SelfMark {
  readonly kind: NoticeKind;
  readonly atMs: number;
}

/** Whether `mark` is the action `notice` reports. */
function marks(mark: SelfMark, notice: AccountNotice): boolean {
  return mark.kind === notice.kind && Math.abs(notice.atMs - mark.atMs) <= SELF_WINDOW_MS;
}

/** Whether `notice` is this device's own action. */
export function isOwnAction(notice: AccountNotice, selfMarks: readonly SelfMark[]): boolean {
  return selfMarks.some((m) => marks(m, notice));
}

/**
 * Split what was fetched into what is shown and what is this device's own
 * action. ONE notice per marker: a marker hides the earliest notice it
 * matches and is then used up, so someone else's action of the same kind in
 * the same ten minutes is still shown. `used` are the markers to forget.
 */
export function splitOwnActions(
  notices: readonly AccountNotice[],
  cursor: number,
  selfMarks: readonly SelfMark[],
): { readonly show: AccountNotice[]; readonly own: AccountNotice[]; readonly used: SelfMark[] } {
  const left = [...selfMarks];
  const show: AccountNotice[] = [];
  const own: AccountNotice[] = [];
  const used: SelfMark[] = [];
  for (const notice of notices.filter((n) => n.id > cursor).sort((a, b) => a.id - b.id)) {
    const at = left.findIndex((m) => marks(m, notice));
    if (at < 0) {
      show.push(notice);
      continue;
    }
    used.push(...left.splice(at, 1));
    own.push(notice);
  }
  return { show, own, used };
}

/**
 * The notices to show, oldest first: those above the cursor that are not
 * this device's own action. Each id is shown once: the caller moves the
 * cursor past a notice when the person answered it (and past the skipped
 * ones at once, {@link cursorAfterSkipped}).
 */
export function noticesToShow(
  notices: readonly AccountNotice[],
  cursor: number,
  selfMarks: readonly SelfMark[],
): AccountNotice[] {
  return splitOwnActions(notices, cursor, selfMarks).show;
}

/**
 * Where the cursor may move at once after a fetch: past every leading notice
 * that is not shown (an own action), up to the first one that is. With
 * nothing to show it moves to `nextAfter`.
 */
export function cursorAfterSkipped(
  page: NoticePage,
  cursor: number,
  selfMarks: readonly SelfMark[],
): number {
  const shown = noticesToShow(page.notices, cursor, selfMarks);
  const first = shown[0];
  if (first === undefined) return Math.max(cursor, page.nextAfter);
  return Math.max(cursor, first.id - 1);
}

/** A device with no cursor asks for the notices of this long back. */
export const FIRST_LANDING_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;

/** How a landing asks: from the cursor, or (a device that has none) from
 *  thirty days ago. */
export type NoticeQuery =
  | { readonly kind: "after"; readonly after: number }
  | { readonly kind: "since"; readonly sinceMs: number };

export function firstNoticeQuery(cursorKnown: boolean, cursor: number, nowMs: number): NoticeQuery {
  return cursorKnown
    ? { kind: "after", after: cursor }
    : { kind: "since", sinceMs: Math.max(0, nowMs - FIRST_LANDING_WINDOW_MS) };
}

/**
 * What one landing does with what it fetched (every page, oldest first).
 *
 * The notices that are not this device's own action are shown; the cursor
 * moves at once past the ones it skips; the markers that hid a notice are
 * used up. A device that had no cursor (it asked for the last thirty days)
 * shows what came back the same way and keeps a cursor from then on.
 */
export function planNotices(input: {
  readonly cursor: number;
  readonly notices: readonly AccountNotice[];
  /** `next_after` of the last page fetched. */
  readonly nextAfter: number;
  readonly marks: readonly SelfMark[];
}): { readonly show: AccountNotice[]; readonly cursor: number; readonly usedMarks: SelfMark[] } {
  const split = splitOwnActions(input.notices, input.cursor, input.marks);
  const first = split.show[0];
  const head = Math.max(input.cursor, input.nextAfter, ...input.notices.map((n) => n.id));
  return {
    show: split.show,
    cursor: first === undefined ? head : Math.max(input.cursor, first.id - 1),
    usedMarks: split.used,
  };
}

/** UX 19.6: reached from a notice, the secure screen's password change is
 *  open this long without another proof. */
export const NOTICE_PASSWORD_OPEN_MS = 10 * 60 * 1000;

/** Whether the secure screen opened at `openedAtMs` (from a notice) still
 *  changes the password without another proof. A clock that went backwards
 *  closes it. */
export function noticePasswordOpen(openedAtMs: number | null, nowMs: number): boolean {
  if (openedAtMs === null) return false;
  const age = nowMs - openedAtMs;
  return age >= 0 && age < NOTICE_PASSWORD_OPEN_MS;
}

/** What one notice says. */
export interface NoticeView {
  /** The title's catalog key; `null` for the one-line notices. */
  readonly titleKey: string | null;
  /** The body's catalog key. Takes `{date}` when `dated`. */
  readonly bodyKey: string;
  readonly dated: boolean;
}

export function noticeView(kind: NoticeKind): NoticeView {
  switch (kind) {
    case "password-changed-by-email":
      return {
        titleKey: "notice.password_by_email.title",
        bodyKey: "notice.password_by_email.body",
        dated: true,
      };
    case "account-opened-by-email":
      return {
        titleKey: "notice.opened_by_email.title",
        bodyKey: "notice.opened_by_email.body",
        dated: true,
      };
    case "recovery-email-added":
      return { titleKey: null, bodyKey: "notice.email_added", dated: false };
    case "recovery-email-replaced":
      return { titleKey: null, bodyKey: "notice.email_replaced", dated: false };
    case "recovery-email-removed":
      return { titleKey: null, bodyKey: "notice.email_removed", dated: false };
    case "recovery-phrase-saved":
      return { titleKey: null, bodyKey: "notice.phrase_saved", dated: false };
  }
}

/** What "It was not me" offers, in order (UX 19.6). */
export type SecureAction = "new-password" | "remove-email" | "save-phrase";

export function secureActions(facts: {
  /** This device holds the account's keys and the account has a username. */
  readonly canReplacePassword: boolean;
  readonly emailEnabled: boolean;
  readonly phraseSaved: boolean;
}): SecureAction[] {
  const out: SecureAction[] = [];
  if (facts.canReplacePassword) out.push("new-password");
  if (facts.emailEnabled) out.push("remove-email");
  if (!facts.phraseSaved) out.push("save-phrase");
  return out;
}

// ---------------------------------------------------------------------------
// Per-device stores
// ---------------------------------------------------------------------------

function cursorKey(accountIdB64: string): string {
  return `lakko:account-notices-seen:${accountIdB64}`;
}

function marksKey(accountIdB64: string): string {
  return `lakko:account-notices-self:${accountIdB64}`;
}

/** The highest notice id this device dealt with; `0` when none. */
export function readNoticeCursor(storage: MarkerStorage | undefined, accountIdB64: string): number {
  try {
    const raw = storage?.getItem(cursorKey(accountIdB64)) ?? null;
    if (raw === null) return 0;
    const n = Number(raw);
    return Number.isSafeInteger(n) && n >= 0 ? n : 0;
  } catch {
    return 0;
  }
}

/** Whether this device has a cursor for the account at all. A device that
 *  has none (its first landing) asks for the last thirty days
 *  ({@link firstNoticeQuery}). */
export function noticeCursorKnown(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return (storage?.getItem(cursorKey(accountIdB64)) ?? null) !== null;
  } catch {
    return false;
  }
}

/** Start the cursor of a device that had none. */
export function startNoticeCursor(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  cursor: number,
): void {
  try {
    if (!noticeCursorKnown(storage, accountIdB64)) {
      storage?.setItem(cursorKey(accountIdB64), String(Math.max(0, cursor)));
    }
  } catch {
    // Storage blocked: older notices are shown once on this device.
  }
}

/** Move the cursor forward. It never moves back. */
export function writeNoticeCursor(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  cursor: number,
): void {
  try {
    if (cursor > readNoticeCursor(storage, accountIdB64)) {
      storage?.setItem(cursorKey(accountIdB64), String(cursor));
    }
  } catch {
    // Storage blocked: the notice is shown again after a reload.
  }
}

export function readSelfMarks(storage: MarkerStorage | undefined, accountIdB64: string): SelfMark[] {
  try {
    const raw = storage?.getItem(marksKey(accountIdB64)) ?? null;
    if (raw === null) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const out: SelfMark[] = [];
    for (const m of parsed as unknown[]) {
      if (typeof m !== "object" || m === null) continue;
      const r = m as Record<string, unknown>;
      if (isKind(r["kind"]) && typeof r["atMs"] === "number" && Number.isFinite(r["atMs"])) {
        out.push({ kind: r["kind"], atMs: r["atMs"] });
      }
    }
    return out;
  } catch {
    return [];
  }
}

/** Marks older than this are dropped: a notice is kept 60 days at most. */
const MARK_RETENTION_MS = 61 * 24 * 60 * 60 * 1000;

/** Remember that this device is performing an action of `kind` now. Written
 *  BEFORE the request that causes the notice, so a notice fetched right
 *  after it is recognised. */
export function markOwnAction(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  kind: NoticeKind,
  nowMs: number,
): void {
  try {
    const kept = readSelfMarks(storage, accountIdB64).filter(
      (m) => nowMs - m.atMs < MARK_RETENTION_MS,
    );
    kept.push({ kind, atMs: nowMs });
    storage?.setItem(marksKey(accountIdB64), JSON.stringify(kept));
  } catch {
    // Storage blocked: this device is told about its own action once.
  }
}

/** Forget the markers that hid a notice: one notice per marker. */
export function consumeSelfMarks(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  used: readonly SelfMark[],
): void {
  if (used.length === 0) return;
  try {
    const left = readSelfMarks(storage, accountIdB64);
    for (const u of used) {
      const at = left.findIndex((m) => m.kind === u.kind && m.atMs === u.atMs);
      if (at >= 0) left.splice(at, 1);
    }
    storage?.setItem(marksKey(accountIdB64), JSON.stringify(left));
  } catch {
    // Storage blocked: a marker may hide one more notice of its kind.
  }
}

/** Forget both stores (sign-out wipe). */
export function clearNoticeStores(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(cursorKey(accountIdB64));
    storage?.removeItem(marksKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}
