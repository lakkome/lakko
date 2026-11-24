// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 9, `docs/ux/group-and-channel-privacy.md` section 9 — the
 * end of a membership: the rules and the one durable record.
 *
 * A person removed from a group loses it: the row and the history go from all
 * their devices; added again, they see only new messages. Leaving does the
 * same, and a channel's unsubscribe does the same.
 *
 * ## The trigger is a state, not an event
 *
 * After a commit is applied, the group is not active AND the frozen roster
 * holds no leaf of this account ({@link endTriggered}). `isActive` alone is
 * wrong: a device whose own leaf was retired while the account stays in the
 * group (ADR-0050) keeps its history and is re-admitted.
 *
 * ## Why a record
 *
 * Ending a membership is several steps across several stores, and the page
 * can close between any two of them. One durable record per conversation
 * (`EndRecord`) names the step reached, so a crash resumes the same work:
 * `recorded` (the end and the notice are on disk) -> `published` (the
 * `conversation_ended` register is folded and queued) -> `stopped` (poll,
 * beacon and push are off) -> `wiped`. The order lives in
 * `membership-end-runtime.ts`.
 *
 * ## What a refused read never does
 *
 * Nothing here runs on a refused Delivery Service read: the only inputs are
 * the state of a group after a commit this device applied, and a register
 * from this account's own devices. The background paths (the service worker)
 * reach none of this (C-59).
 *
 * Pure apart from {@link MembershipEndStore}'s backend; the sealed row is
 * `membership-end-idb.ts`.
 */

/** A conversation's kind on the wire: `2` a group, `3` a channel. A one-to-one
 *  (`1`) never ends this way (ADR-0062: its membership is fixed). */
export type EndRefKind = 2 | 3;

/** How far an end got. */
export type EndStage = "recorded" | "published" | "stopped" | "wiped";

/** What set the end off here. */
export type EndOrigin =
  /** This device applied the commit that ended it, or unsubscribed. */
  | "applied"
  /** This device heard of it from the account's `conversation_ended` register. */
  | "heard";

/** One conversation whose end is being carried out on this device. */
export interface EndRecord {
  /** {@link endKey}. */
  readonly key: string;
  readonly refKind: EndRefKind;
  /** Lower-case hex of the conversation's id: the group id, or the channel id. */
  readonly idHex: string;
  /** The groups of the conversation this device holds, standard base64, the
   *  head LAST. Empty for a channel. */
  readonly segmentsB64: readonly string[];
  /** The ids a route can name it by. */
  readonly routeIdsB64: readonly string[];
  /** The name, for the notice. Empty when none is known. */
  readonly title: string;
  /** This device asked to leave (or unsubscribed): no notice. */
  readonly left: boolean;
  readonly origin: EndOrigin;
  /** The MLS epoch of the end, decimal; `0` for a channel. */
  readonly endedEpoch: string;
  readonly stage: EndStage;
  /** The synced registers of the conversation were cleared. */
  readonly syncedCleared: boolean;
  readonly atMs: number;
}

/** "You are no longer in {name}.": in memory only (UX section 9, 2026-11-06:
 *  nothing is kept for later), until its dialog or notice has been shown. */
/**
 * UX `group-and-channel-privacy.md` 9.2 — the text to put back into the
 * composer when the person is added again while the end's dialog still showed
 * it: the dialog's draft, if it held one that is not only white space.
 */
export function draftToRestore(notice: Pick<EndNotice, "draft"> | undefined): string | null {
  const draft = notice?.draft ?? null;
  return draft !== null && draft.trim().length > 0 ? draft : null;
}

export interface EndNotice {
  readonly key: string;
  readonly title: string;
  /** The ids a route can name the conversation by, standard base64 for a
   *  group (its groups and its conversation id); empty for a channel. */
  readonly routeIdsB64: readonly string[];
  /** Text the composer held when the conversation ended there, or `null`. */
  readonly draft: string | null;
  readonly atMs: number;
}

/**
 * A Welcome the core refused because this device still holds that group as
 * active. The mailbox cursor has moved past it, so this is the only copy: its
 * group's journal is read, and it is tried again.
 */
export interface HeldWelcome {
  readonly messageId: number;
  readonly groupIdB64: string;
  readonly welcomeB64: string;
  readonly heldAtMs: number;
}

/** Most Welcomes held (chosen): a bound on a list a stranger can add to, not
 *  a number anyone reaches. Past it the oldest is dropped. */
export const MAX_HELD_WELCOMES = 32;

/** The record's key: the conversation, by kind and id. */
export function endKey(refKind: EndRefKind, idHex: string): string {
  return `${refKind}:${idHex}`;
}

// ---------- the rules ------------------------------------------------------

/**
 * Whether this account's membership of a group is over.
 *
 * Every unknown answers `false`, and that is the direction this has to fail:
 * a torn read is not evidence of anything, and acting on it deletes a live
 * conversation. The ended state is terminal, so a read that failed now is
 * asked again at the next commit and at the next launch.
 *
 * A one-to-one is never ended here. ADR-0062 fixes its membership, so its own
 * leaf going is a device of the account retiring; leaving one stays a local
 * delete on the contact card.
 */
export function endTriggered(input: {
  readonly kind: "chat" | "group";
  readonly isActive: boolean | undefined;
  /** The people in the frozen roster, `null` on a torn read. */
  readonly rosterPersonKeysB64: readonly string[] | null;
  /** Every key this account's leaves can be bound to. */
  readonly ownPersonKeysB64: readonly string[];
}): boolean {
  if (input.kind !== "group") return false;
  if (input.isActive !== false) return false;
  const roster = input.rosterPersonKeysB64;
  if (roster === null || roster.length === 0) return false;
  if (input.ownPersonKeysB64.length === 0) return false;
  return !input.ownPersonKeysB64.some((k) => roster.includes(k));
}

/** What a heard `conversation_ended` register means for a group held here. */
export type HeardVerdict =
  /** This device came back after that end: stop nothing, wipe nothing, say so. */
  | "came-back"
  /** Carry out the end. */
  | "end"
  /** Nothing to act on: no such group, or a handle that cannot be read. */
  | "nothing";

/**
 * Judge a heard register. Compares epochs BEFORE anything stops: the delete
 * refuses a rejoined group only at its last step, and by then the poll and the
 * push subscription would already be gone.
 */
export function heardRegisterVerdict(input: {
  readonly held: boolean;
  readonly isActive: boolean | undefined;
  readonly epoch: bigint | undefined;
  readonly registerEpoch: bigint;
}): HeardVerdict {
  if (!input.held || input.isActive === undefined) return "nothing";
  if (!input.isActive) return "end";
  if (input.epoch === undefined) return "nothing";
  return input.epoch > input.registerEpoch ? "came-back" : "end";
}

/**
 * The key a chat view is mounted under: the group, and which time this device
 * joined it. A Welcome replacing a group under the same id is a new
 * conversation to a view holding the old one's rows and cursor, so it must
 * remount; an ADR-0040 reset replaces the registry entry and must not, and it
 * leaves the generation alone.
 */
export function chatViewKey(entry: {
  readonly groupIdB64: string;
  readonly joinGeneration: number;
}): string {
  return `${entry.joinGeneration}:${entry.groupIdB64}`;
}

/** The group a {@link chatViewKey} names. */
export function groupIdOfViewKey(key: string): string {
  return key.slice(key.indexOf(":") + 1);
}

/** Walks wrappers the way `refusalReasonOf` does; `ContactsError.wrap` parks
 *  the wasm error on `variant.cause`. */
const MAX_WRAP_DEPTH = 8;

/**
 * The group a `groupStillActive` refusal names (raw id bytes), or `null` for
 * any other failure. Read from the error's own property, never parsed from
 * its message.
 */
export function stillActiveGroupIdOf(err: unknown): Uint8Array | null {
  let current: unknown = err;
  for (let depth = 0; depth < MAX_WRAP_DEPTH; depth++) {
    if (typeof current !== "object" || current === null) return null;
    const { refusalReason, refusalGroupId } = current as {
      refusalReason?: unknown;
      refusalGroupId?: unknown;
    };
    if (typeof refusalReason === "string") {
      if (refusalReason !== "groupStillActive") return null;
      return refusalGroupId instanceof Uint8Array ? refusalGroupId : null;
    }
    const wrapped = current as { cause?: unknown; variant?: { cause?: unknown } };
    current = wrapped.variant?.cause ?? wrapped.cause;
  }
  return null;
}

/** What to do with a held Welcome after its group's journal was read. */
export type HeldWelcomeStep =
  /** Keep it: the read did not reach the head. Tried again later. */
  | "keep"
  /** Read to the head and the group is still active: a duplicate or a forgery. */
  | "drop";

/** The verdict on a Welcome refused AGAIN after its journal was read. The core
 *  does not report a Welcome's epoch, so no client can tell sooner. */
export function heldWelcomeStep(journalRead: boolean): HeldWelcomeStep {
  return journalRead ? "drop" : "keep";
}

// ---------- the stored row -------------------------------------------------

/** Row schema version. There is no migration (no production data exists). */
export const MEMBERSHIP_END_ROW_VERSION = 1;

export interface MembershipEndRow {
  readonly records: readonly EndRecord[];
  readonly welcomes: readonly HeldWelcome[];
}

export const EMPTY_MEMBERSHIP_END_ROW: MembershipEndRow = { records: [], welcomes: [] };

export function encodeMembershipEndRow(row: MembershipEndRow): string {
  return JSON.stringify({ v: MEMBERSHIP_END_ROW_VERSION, ...row });
}

const STAGES: readonly EndStage[] = ["recorded", "published", "stopped", "wiped"];

function isRecord(raw: unknown): raw is EndRecord {
  if (typeof raw !== "object" || raw === null) return false;
  const r = raw as Partial<EndRecord>;
  return (
    typeof r.key === "string" &&
    (r.refKind === 2 || r.refKind === 3) &&
    typeof r.idHex === "string" &&
    Array.isArray(r.segmentsB64) &&
    r.segmentsB64.every((s) => typeof s === "string") &&
    Array.isArray(r.routeIdsB64) &&
    r.routeIdsB64.every((s) => typeof s === "string") &&
    typeof r.title === "string" &&
    typeof r.left === "boolean" &&
    (r.origin === "applied" || r.origin === "heard") &&
    typeof r.endedEpoch === "string" &&
    /^[0-9]+$/.test(r.endedEpoch) &&
    typeof r.stage === "string" &&
    STAGES.includes(r.stage) &&
    typeof r.syncedCleared === "boolean" &&
    typeof r.atMs === "number"
  );
}

function isWelcome(raw: unknown): raw is HeldWelcome {
  if (typeof raw !== "object" || raw === null) return false;
  const w = raw as Partial<HeldWelcome>;
  return (
    typeof w.messageId === "number" &&
    typeof w.groupIdB64 === "string" &&
    typeof w.welcomeB64 === "string" &&
    typeof w.heldAtMs === "number"
  );
}

/** Decode a stored row. `null` for a row of another version; a malformed
 *  entry is dropped alone. */
export function decodeMembershipEndRow(json: string): MembershipEndRow | null {
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as { v?: unknown; records?: unknown; welcomes?: unknown };
  if (r.v !== MEMBERSHIP_END_ROW_VERSION) return null;
  return {
    records: Array.isArray(r.records) ? r.records.filter(isRecord) : [],
    welcomes: Array.isArray(r.welcomes) ? r.welcomes.filter(isWelcome) : [],
  };
}

// ---------- the store ------------------------------------------------------

/** Where the row is kept. */
export interface MembershipEndBackend {
  load(accountId: string): Promise<MembershipEndRow | null>;
  save(accountId: string, row: MembershipEndRow): Promise<void>;
}

/**
 * The row in memory, every change written through and awaited.
 *
 * Every mutator resolves once the row is on disk and rejects when it could not
 * be written, which is what makes a stage durable before the next step runs:
 * a caller that gets a rejection does not go on. Writes for one account run
 * one at a time, in order, and each writes the newest row.
 */
export class MembershipEndStore {
  private row: MembershipEndRow = EMPTY_MEMBERSHIP_END_ROW;
  private account: string | null = null;
  private chain: Promise<unknown> = Promise.resolve();
  private readonly listeners = new Set<() => void>();

  constructor(private readonly backend: MembershipEndBackend) {}

  /** Bind to an account and load its row. */
  async open(accountId: string): Promise<void> {
    this.account = accountId;
    this.row = EMPTY_MEMBERSHIP_END_ROW;
    let loaded: MembershipEndRow | null = null;
    try {
      loaded = await this.backend.load(accountId);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[membership-end] row unreadable", err instanceof Error ? err.name : "");
    }
    if (this.account !== accountId) return;
    this.row = loaded ?? EMPTY_MEMBERSHIP_END_ROW;
    this.notify();
  }

  /** Forget everything in memory (sign-out). The row on disk stays. */
  close(): void {
    this.account = null;
    this.row = EMPTY_MEMBERSHIP_END_ROW;
    this.notify();
  }

  boundAccount(): string | null {
    return this.account;
  }

  records(): readonly EndRecord[] {
    return this.row.records;
  }

  record(key: string): EndRecord | undefined {
    return this.row.records.find((r) => r.key === key);
  }

  welcomes(): readonly HeldWelcome[] {
    return this.row.welcomes;
  }

  /** Called on any change. */
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** Write a record. */
  begin(record: EndRecord): Promise<void> {
    return this.change((row) => ({
      ...row,
      records: [...row.records.filter((r) => r.key !== record.key), record],
    }));
  }

  /** Replace a record. */
  update(record: EndRecord): Promise<void> {
    return this.change((row) => ({
      ...row,
      records: row.records.map((r) => (r.key === record.key ? record : r)),
    }));
  }

  /** The end is carried out: the record goes. */
  finish(key: string): Promise<void> {
    return this.change((row) => ({ ...row, records: row.records.filter((r) => r.key !== key) }));
  }

  /** Keep a Welcome. A row already held is not held twice; past
   *  {@link MAX_HELD_WELCOMES} the oldest goes. */
  holdWelcome(welcome: HeldWelcome): Promise<void> {
    return this.change((row) => {
      if (row.welcomes.some((w) => w.messageId === welcome.messageId)) return row;
      return { ...row, welcomes: [...row.welcomes, welcome].slice(-MAX_HELD_WELCOMES) };
    });
  }

  dropWelcome(messageId: number): Promise<void> {
    return this.change((row) => ({
      ...row,
      welcomes: row.welcomes.filter((w) => w.messageId !== messageId),
    }));
  }

  /** Change the row and write it. The account is read now: a change made for
   *  an account that signed out meanwhile writes nothing. */
  private change(fn: (row: MembershipEndRow) => MembershipEndRow): Promise<void> {
    const account = this.account;
    if (account === null) return Promise.reject(new Error("membership-end store is not bound"));
    const next = fn(this.row);
    if (next === this.row) return Promise.resolve();
    this.row = next;
    this.notify();
    const run = this.chain.then(async () => {
      // Written newest-first: a burst of changes costs writes of the same row,
      // never an older one after a newer one.
      if (this.account !== account) throw new Error("membership-end store changed account");
      await this.backend.save(account, this.row);
    });
    this.chain = run.catch(() => undefined);
    return run;
  }

  private notify(): void {
    for (const fn of this.listeners) fn();
  }
}
