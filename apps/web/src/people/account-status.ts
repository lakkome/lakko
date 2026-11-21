// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 — what this device knows about whether a person's account still
 * exists, kept per person (keyed on the account key a conversation resolves
 * them to, C-68), and the shared rules that turn it into what is shown.
 *
 * Two sources write here:
 *
 *   * a verified account-deleted statement (push, section 1). Not final
 *     (security review 2026-11-21): recording one clears the recorded
 *     directory answer, so only an answer received after it counts, and a
 *     later `same-person` answer brings the person back;
 *   * a directory answer for the person's account id (pull, section 2).
 *
 * What a record does is the core's `deletionEffects`: show "Deleted
 * account", remove the person's leaves from groups, drop messages held for
 * them. The destructive two need a verified statement AND an `absent`
 * answer, so neither a directory nor a forged statement alone can cause
 * them. Nothing here restates the rule (wasm rules port
 * {@link AccountStatusRules}). The row codec and the in-memory store are pure so they run under
 * vitest; persistence is `account-status-idb.ts`.
 */

/** A directory answer as this device records it. */
export type DirectoryAnswerName = "absent" | "same-person" | "other-key";

/** What is kept for one person. */
export interface PersonStatusRecord {
  /** A verified account-deleted statement arrived. Not final: the effects
   *  are `deletionEffects` of it and the latest answer. */
  readonly statementVerified: boolean;
  readonly lastDirectoryAnswer?: DirectoryAnswerName | undefined;
  readonly lastAnswerAtMs?: number | undefined;
  /** When the directory last said the account still exists, as this person. */
  readonly lastSamePersonAtMs?: number | undefined;
  /** When this device last asked the directory about this person. */
  readonly lastAttemptAtMs?: number | undefined;
  /** The person's 16-byte account id, 32 lower-case hex, when known. */
  readonly accountIdHex?: string | undefined;
  /** ADR-0098 section 5: groups (base64 id) where this device, as owner or
   *  admin, owes or already sent the removal of this person's leaves. */
  readonly removals?: Readonly<Record<string, "owed" | "proposed">> | undefined;
}

/** The empty record. */
export const UNKNOWN_PERSON: PersonStatusRecord = { statementVerified: false };

/**
 * The wasm exports the rules need, structurally (so a test can hand in the
 * real module or a fake).
 */
export interface AccountStatusRules {
  accountDeletionStatus(statement_verified: boolean, last_directory_answer?: number | null): number;
  deletionEffects(
    statement_verified: boolean,
    last_directory_answer?: number | null,
  ): { readonly showDeleted: boolean; readonly removeFromGroups: boolean; readonly dropHeldSends: boolean; free(): void };
  mayShowUsername(status: number, confirmed_at_ms: bigint | null | undefined, now_ms: bigint): boolean;
  directoryCheckDue(
    statement_verified: boolean,
    last_answer_at_ms: bigint | null | undefined,
    last_attempt_at_ms: bigint | null | undefined,
    now_ms: bigint,
  ): boolean;
  readonly DirectoryAnswer: { readonly Absent: number; readonly SamePerson: number; readonly OtherKey: number };
  readonly AccountStatus: { readonly Active: number; readonly Deleted: number };
}

function big(ms: number | undefined): bigint | undefined {
  return ms === undefined || !Number.isFinite(ms) || ms < 0 ? undefined : BigInt(Math.trunc(ms));
}

function answerOf(rules: AccountStatusRules, name: DirectoryAnswerName | undefined): number | undefined {
  switch (name) {
    case "absent":
      return rules.DirectoryAnswer.Absent;
    case "same-person":
      return rules.DirectoryAnswer.SamePerson;
    case "other-key":
      return rules.DirectoryAnswer.OtherKey;
    default:
      return undefined;
  }
}

/** The status the shared rule gives the record. */
function statusOf(rules: AccountStatusRules, rec: PersonStatusRecord | undefined): number {
  const r = rec ?? UNKNOWN_PERSON;
  return rules.accountDeletionStatus(r.statementVerified, answerOf(rules, r.lastDirectoryAnswer) ?? null);
}

/** What a deletion does on this device (the core's `deletionEffects`). */
export interface DeletionEffectsView {
  /** Show "Deleted account" in place of the person's name. */
  readonly showDeleted: boolean;
  /** An owner's or admin's device proposes the removal of the person's leaves. */
  readonly removeFromGroups: boolean;
  /** Messages held for the person, never sent, may be dropped. */
  readonly dropHeldSends: boolean;
}

/** `deletionEffects` of the record. */
export function deletionEffectsOf(rules: AccountStatusRules, rec: PersonStatusRecord | undefined): DeletionEffectsView {
  const r = rec ?? UNKNOWN_PERSON;
  const e = rules.deletionEffects(r.statementVerified, answerOf(rules, r.lastDirectoryAnswer) ?? null);
  try {
    return { showDeleted: e.showDeleted, removeFromGroups: e.removeFromGroups, dropHeldSends: e.dropHeldSends };
  } finally {
    e.free();
  }
}

/** `deletionEffects(...).showDeleted`: shown as "Deleted account". */
export function isDeletedPerson(rules: AccountStatusRules, rec: PersonStatusRecord | undefined): boolean {
  return deletionEffectsOf(rules, rec).showDeleted;
}

/** `mayShowUsername`: a `@username` may be drawn beside this person now. The
 *  caller still requires the ADR-0060 certificate to verify. */
export function usernameShowable(
  rules: AccountStatusRules,
  rec: PersonStatusRecord | undefined,
  nowMs: number,
): boolean {
  return rules.mayShowUsername(statusOf(rules, rec), big(rec?.lastSamePersonAtMs) ?? null, BigInt(Math.trunc(nowMs)));
}

/** `directoryCheckDue`: whether this person may be asked about now. */
export function directoryCheckIsDue(
  rules: AccountStatusRules,
  rec: PersonStatusRecord | undefined,
  nowMs: number,
): boolean {
  const r = rec ?? UNKNOWN_PERSON;
  return rules.directoryCheckDue(
    r.statementVerified,
    big(r.lastAnswerAtMs) ?? null,
    big(r.lastAttemptAtMs) ?? null,
    BigInt(Math.trunc(nowMs)),
  );
}

/**
 * `rec` with a verified statement recorded. The recorded directory answer
 * and the last attempt are cleared: only an answer received after the
 * statement counts, and the directory is asked again at once.
 */
export function withStatement(rec: PersonStatusRecord | undefined, accountIdHex?: string): PersonStatusRecord {
  const r = rec ?? UNKNOWN_PERSON;
  const { lastDirectoryAnswer: _a, lastAnswerAtMs: _t, lastAttemptAtMs: _x, ...rest } = r;
  return { ...rest, statementVerified: true, ...(accountIdHex === undefined ? {} : { accountIdHex }) };
}

/** `rec` with a directory answer recorded at `nowMs`. */
export function withAnswer(
  rec: PersonStatusRecord | undefined,
  answer: DirectoryAnswerName,
  nowMs: number,
  accountIdHex?: string,
): PersonStatusRecord {
  const r = rec ?? UNKNOWN_PERSON;
  return {
    ...r,
    lastDirectoryAnswer: answer,
    lastAnswerAtMs: nowMs,
    ...(answer === "same-person" ? { lastSamePersonAtMs: nowMs } : {}),
    ...(accountIdHex === undefined ? {} : { accountIdHex }),
  };
}

/** `rec` with an attempt recorded at `nowMs`, set BEFORE the request. */
export function withAttempt(rec: PersonStatusRecord | undefined, nowMs: number): PersonStatusRecord {
  return { ...(rec ?? UNKNOWN_PERSON), lastAttemptAtMs: nowMs };
}

/** `rec` with the removal in `groupIdB64` marked. */
export function withRemoval(
  rec: PersonStatusRecord | undefined,
  groupIdB64: string,
  state: "owed" | "proposed",
): PersonStatusRecord {
  const r = rec ?? UNKNOWN_PERSON;
  return { ...r, removals: { ...(r.removals ?? {}), [groupIdB64]: state } };
}

// ---------------------------------------------------------------------------
// Directory answer
// ---------------------------------------------------------------------------

/** The parts of an identity bundle the classification reads. */
export interface IdentityBundleFacts {
  /** base64 of the bundle's current account key. */
  readonly signaturePubkeyB64: string;
  /** base64 of the two-way conversation link, when the bundle carries one. */
  readonly conversationLinkB64?: string | undefined;
}

/**
 * ADR-0098 section 2: what one answer of `GET /discovery/identity/{id}` means
 * for a person this device holds `heldKeyB64` for. `bundle === null` is the
 * `404`. `verifyLink` is the core's `verifyConversationLink(link, current)`
 * (returns the conversation key the link names; throws when it does not
 * verify).
 */
export function classifyIdentityAnswer(
  heldKeyB64: string,
  bundle: IdentityBundleFacts | null,
  verifyLink: (link: Uint8Array, current: Uint8Array) => Uint8Array,
  decode: (b64: string) => Uint8Array,
  encode: (bytes: Uint8Array) => string,
): DirectoryAnswerName {
  if (bundle === null) return "absent";
  if (bundle.signaturePubkeyB64 === heldKeyB64) return "same-person";
  const link = bundle.conversationLinkB64;
  if (link !== undefined && link.length > 0) {
    try {
      const named = verifyLink(decode(link), decode(bundle.signaturePubkeyB64));
      if (encode(named) === heldKeyB64) return "same-person";
    } catch {
      // A link that does not verify names nobody.
    }
  }
  return "other-key";
}

// ---------------------------------------------------------------------------
// Row codec
// ---------------------------------------------------------------------------

/** Schema version of the JSON inside the encrypted row. */
export const PERSON_STATUS_ROW_VERSION = 1;

const ANSWERS: readonly DirectoryAnswerName[] = ["absent", "same-person", "other-key"];

function finiteMs(x: unknown): number | undefined {
  return typeof x === "number" && Number.isSafeInteger(x) && x >= 0 ? x : undefined;
}

/** JSON of every record. */
export function encodePersonStatusRows(rows: ReadonlyMap<string, PersonStatusRecord>): string {
  const p: Record<string, unknown> = {};
  for (const [key, r] of rows) {
    p[key] = {
      s: r.statementVerified,
      a: r.lastDirectoryAnswer ?? null,
      at: r.lastAnswerAtMs ?? null,
      sp: r.lastSamePersonAtMs ?? null,
      tr: r.lastAttemptAtMs ?? null,
      id: r.accountIdHex ?? null,
      rm: r.removals ?? null,
    };
  }
  return JSON.stringify({ v: PERSON_STATUS_ROW_VERSION, p });
}

/** The records in a row; an unreadable row or entry is skipped. */
export function decodePersonStatusRows(json: string): Map<string, PersonStatusRecord> {
  const out = new Map<string, PersonStatusRecord>();
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    return out;
  }
  if (typeof raw !== "object" || raw === null) return out;
  if ((raw as { v?: unknown }).v !== PERSON_STATUS_ROW_VERSION) return out;
  const p = (raw as { p?: unknown }).p;
  if (typeof p !== "object" || p === null) return out;
  for (const [key, value] of Object.entries(p as Record<string, unknown>)) {
    if (typeof value !== "object" || value === null) continue;
    const v = value as Record<string, unknown>;
    if (typeof v.s !== "boolean") continue;
    const answer = ANSWERS.find((a) => a === v.a);
    const removals: Record<string, "owed" | "proposed"> = {};
    if (typeof v.rm === "object" && v.rm !== null) {
      for (const [g, st] of Object.entries(v.rm as Record<string, unknown>)) {
        if (st === "owed" || st === "proposed") removals[g] = st;
      }
    }
    const idHex = typeof v.id === "string" && /^[0-9a-f]{32}$/.test(v.id) ? v.id : undefined;
    out.set(key, {
      statementVerified: v.s,
      ...(answer === undefined ? {} : { lastDirectoryAnswer: answer }),
      ...(finiteMs(v.at) === undefined ? {} : { lastAnswerAtMs: finiteMs(v.at) }),
      ...(finiteMs(v.sp) === undefined ? {} : { lastSamePersonAtMs: finiteMs(v.sp) }),
      ...(finiteMs(v.tr) === undefined ? {} : { lastAttemptAtMs: finiteMs(v.tr) }),
      ...(idHex === undefined ? {} : { accountIdHex: idHex }),
      ...(Object.keys(removals).length === 0 ? {} : { removals }),
    });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/** Where the records live between sessions. One row per account. */
export interface PersonStatusBackend {
  load(accountId: string): Promise<ReadonlyMap<string, PersonStatusRecord> | null>;
  save(accountId: string, rows: ReadonlyMap<string, PersonStatusRecord>): Promise<void>;
}

/** A backend that keeps nothing (tests, a device with no key). */
export const MEMORY_PERSON_STATUS_BACKEND: PersonStatusBackend = {
  load: async () => null,
  save: async () => undefined,
};

/** One account's records in memory, written through to the backend. */
export class PersonStatusStore {
  private rows = new Map<string, PersonStatusRecord>();
  private account: string | null = null;
  private readonly listeners = new Set<() => void>();
  private chain: Promise<void> = Promise.resolve();

  constructor(
    private readonly backend: PersonStatusBackend,
    private readonly onPersistError: (err: unknown) => void = () => undefined,
  ) {}

  /** The bound account, or `null`. */
  boundAccount(): string | null {
    return this.account;
  }

  /** Load the account's row; records written meanwhile are kept. */
  async open(accountId: string): Promise<void> {
    this.account = accountId;
    this.rows = new Map();
    let loaded: ReadonlyMap<string, PersonStatusRecord> | null = null;
    try {
      loaded = await this.backend.load(accountId);
    } catch (err) {
      this.onPersistError(err);
    }
    if (this.account !== accountId) return;
    const next = new Map(loaded ?? []);
    for (const [k, v] of this.rows) next.set(k, v);
    this.rows = next;
    this.notify();
  }

  /** Forget everything in memory (sign-out). The row on disk stays. */
  close(): void {
    this.account = null;
    this.rows = new Map();
    this.notify();
  }

  get(keyB64: string): PersonStatusRecord | undefined {
    return this.rows.get(keyB64);
  }

  entries(): IterableIterator<[string, PersonStatusRecord]> {
    return this.rows.entries();
  }

  /** Replace one person's record and persist (when an account is bound). */
  update(keyB64: string, f: (rec: PersonStatusRecord | undefined) => PersonStatusRecord): Promise<void> {
    const account = this.account;
    if (account === null) return Promise.resolve();
    this.rows = new Map(this.rows).set(keyB64, f(this.rows.get(keyB64)));
    this.notify();
    const snapshot: ReadonlyMap<string, PersonStatusRecord> = this.rows;
    this.chain = this.chain
      .then(() => this.backend.save(account, snapshot))
      .catch((err: unknown) => {
        this.onPersistError(err);
      });
    return this.chain;
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private notify(): void {
    for (const fn of this.listeners) fn();
  }
}
