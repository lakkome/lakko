// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0051 §1-§3 — the per-`(account, group)` shared group name on this
 * device, and the RECEIVE-SIDE decision that guards it.
 *
 * Three things live here, all free of wasm, Solid and IndexedDB so `vitest`
 * can pin the rules that could actually be wrong:
 *
 *   * {@link groupNameVerdict} — the pure gate an incoming envelope passes
 *     before it may replace what is stored. It copies the shape of the
 *     push-routing verdict (`push/group-routing.ts`): identical first, then
 *     authority read off the LIVE role table, then the order. A name is one
 *     person's assertion about the whole group, so the sender's authority is
 *     the receiver's check, never the sender's claim (UX spec §5).
 *   * the row codec ({@link encodeGroupNameRow} / {@link decodeGroupNameRow})
 *     — the JSON inside the encrypted IndexedDB row `group-name-idb.ts`
 *     writes. A `u64` stamp is a decimal string because it does not survive
 *     `JSON.parse` as a number.
 *   * {@link GroupNameStore} — the in-memory map + persistence, keyed on the
 *     base64 group id, one row per account.
 *
 * ## Why an empty name is stored, not deleted
 *
 * "Empty clears the name" (ADR-0051 §1). A clear stamped T must still beat a
 * growth re-send of the OLD name stamped T-1, so the row stays with `name ===
 * ""` and its stamp; {@link GroupNameStore.nameOf} reads that as "no name".
 * Deleting the row would make the next stale re-send look like a first
 * observation and resurrect the cleared name.
 *
 * ## What the verdict deliberately does NOT do
 *
 * It does not clamp the stamp against anything. The push-routing secret
 * stamps with the MLS epoch, which every receiver can bound; a group name
 * stamps with the setter's clock (the profile precedent, ADR-0048), which
 * nobody can bound. The cost is that an authorised sender with a wrong clock
 * can set a name that is hard to override until a later stamp — the same
 * property the profile name already has, and it is reversible by the same
 * tier (owner or admin) setting it again.
 */
import { t } from "../l10n";

/** A stored or incoming shared name. `name === ""` means "cleared". */
export interface StoredGroupName {
  readonly name: string;
  /** Setter-clock UNIX milliseconds, as stamped in the envelope. */
  readonly updatedAtMs: bigint;
}

/** Outcome of {@link groupNameVerdict}. Mirrors the push-routing shape. */
export type GroupNameVerdict =
  /** Byte-identical to what is stored (same name, same stamp): no write. */
  | "accept-identical"
  /** The sender is the ADR-0022 owner and the name is newer. */
  | "accept-owner"
  /** The sender is an ADR-0022 admin and the name is newer. */
  | "accept-admin"
  /** The group carries no role table (legacy / open group): any member,
   *  when newer. */
  | "accept-no-owner"
  /** A plain member tried to rename the group. Dropped. */
  | "reject-not-authorized"
  /** The role table / roster could not be read, the sender could not be
   *  resolved, or the local row could not be read to order against.
   *  Dropped (fail-closed, invariant #4). */
  | "reject-unverifiable"
  /** Authorised, but not strictly newer than what is stored. Dropped —
   *  this is what keeps a growth re-send from clobbering a later rename. */
  | "reject-stale";

/** `true` for every `accept-*` verdict. */
export function groupNameVerdictAccepts(v: GroupNameVerdict): boolean {
  return v.startsWith("accept-");
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

function containsKey(keys: readonly Uint8Array[], key: Uint8Array): boolean {
  return keys.some((k) => bytesEqual(k, key));
}

/**
 * The pure receive-side decision (ADR-0051 §2).
 *
 * | role table | sender            | stored          | verdict                 |
 * |------------|-------------------|-----------------|-------------------------|
 * | any        | anyone            | identical       | `accept-identical`      |
 * | unreadable | anyone            | any             | `reject-unverifiable`   |
 * | present    | owner             | none / older    | `accept-owner`          |
 * | present    | admin             | none / older    | `accept-admin`          |
 * | present    | plain member      | any             | `reject-not-authorized` |
 * | present    | unresolvable      | any             | `reject-unverifiable`   |
 * | absent     | anyone            | none / older    | `accept-no-owner`       |
 * | any        | authorised        | newer or equal  | `reject-stale`          |
 *
 * @param isNewer MUST be the wasm `groupNameIsNewer` export, threaded in so
 *   the order stays defined once in the core (strictly greater; equal is not
 *   newer).
 */
export function groupNameVerdict(args: {
  /** `group.roleTableOwner()` — `undefined` = legacy / open group. */
  ownerKey: Uint8Array | undefined;
  /** `group.roleTableAdmins()` sliced into 32-byte keys. Only consulted when
   *  `ownerKey` is defined. */
  adminKeys: readonly Uint8Array[];
  /** The sender's ACCOUNT key resolved at receipt (C-68), `undefined` when
   *  the leaf could not be resolved. */
  senderKey: Uint8Array | undefined;
  /** What this device holds for the group, `null` when nothing. */
  stored: StoredGroupName | null;
  /** Set when the local row could not be read. There is then nothing to
   *  order against, so a changed name is refused. */
  storedUnreadable: boolean;
  /** The decoded incoming envelope. */
  incoming: StoredGroupName;
  /** Set when reading the role table / roster threw. */
  verifyFailed: boolean;
  /** The canonical order from the wasm core. */
  isNewer: (incomingUpdatedAtMs: bigint, storedUpdatedAtMs: bigint) => boolean;
}): GroupNameVerdict {
  // (1) Idempotent case first: a re-observed identical name (a growth
  // re-send of exactly what is held) changes nothing, so it needs neither
  // authority nor ordering, and it must not fail a device whose table read
  // happened to tear.
  if (
    args.stored !== null &&
    args.stored.name === args.incoming.name &&
    args.stored.updatedAtMs === args.incoming.updatedAtMs
  ) {
    return "accept-identical";
  }
  // From here the envelope would CHANGE this device's title.
  if (args.verifyFailed) return "reject-unverifiable";
  if (args.storedUnreadable) return "reject-unverifiable";
  // (2) Authority, read off the live role table.
  const authorised = ((): GroupNameVerdict => {
    if (args.ownerKey === undefined) return "accept-no-owner";
    if (args.senderKey === undefined) return "reject-unverifiable";
    if (bytesEqual(args.senderKey, args.ownerKey)) return "accept-owner";
    if (containsKey(args.adminKeys, args.senderKey)) return "accept-admin";
    return "reject-not-authorized";
  })();
  if (!groupNameVerdictAccepts(authorised)) return authorised;
  // (3) Nothing stored to order against.
  if (args.stored === null) return authorised;
  // (4) The order decides WHICH authorised write survives.
  return args.isNewer(args.incoming.updatedAtMs, args.stored.updatedAtMs)
    ? authorised
    : "reject-stale";
}

// ---------------------------------------------------------------------------
// Copy (UX spec §5)
// ---------------------------------------------------------------------------

/** The row label. */
export const GROUP_NAME_LABEL = (): string => t("chat.group_name.label");
/** The Save button. */
export const GROUP_NAME_SAVE = (): string => t("common.save");
/** Shown when a stale edit control was used by someone who may not rename. */
export const GROUP_NAME_ONLY_ADMINS = (): string => t("chat.group_name.only_admins");

/**
 * Plain copy for a `groupNameError` code the composer throws (the profile
 * display-name rule, ADR-0048 §4). Unknown codes fall back to one sentence.
 */
export function groupNameErrorText(code: string | undefined): string {
  switch (code) {
    case "name.tooLong":
    case "name.tooManyBytes":
      return t("chat.group_name.error.too_long");
    case "name.lineBreak":
      return t("chat.group_name.error.line_break");
    case "name.controlOrFormatCharacter":
      return t("chat.group_name.error.character");
    default:
      return t("chat.group_name.error.generic");
  }
}

/** The `groupNameError` code off a composer throw, or `undefined`. */
export function groupNameErrorCode(err: unknown): string | undefined {
  if (typeof err !== "object" || err === null) return undefined;
  const code = (err as { groupNameError?: unknown }).groupNameError;
  return typeof code === "string" ? code : undefined;
}

// ---------------------------------------------------------------------------
// Row codec
// ---------------------------------------------------------------------------

/** Schema version of the JSON payload inside the encrypted row. */
export const GROUP_NAME_ROW_VERSION = 1;

/** The JSON shape inside the ciphertext. */
export interface GroupNameRowPayload {
  readonly v: 1;
  /** base64 group id -> `{ n: name, t: decimal updated_at_ms }`. */
  readonly g: Record<string, { readonly n: string; readonly t: string }>;
}

/** Encode the rows to the payload JSON. */
export function encodeGroupNameRow(rows: ReadonlyMap<string, StoredGroupName>): string {
  const g: Record<string, { n: string; t: string }> = {};
  for (const [groupIdB64, v] of rows) g[groupIdB64] = { n: v.name, t: v.updatedAtMs.toString() };
  const payload: GroupNameRowPayload = { v: GROUP_NAME_ROW_VERSION, g };
  return JSON.stringify(payload);
}

/** Decode payload JSON, dropping any malformed entry rather than trusting it.
 *  `null` for a payload that is not this schema at all. */
export function decodeGroupNameRow(json: string): Map<string, StoredGroupName> | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<GroupNameRowPayload>;
  if (obj.v !== GROUP_NAME_ROW_VERSION) return null;
  const rows = new Map<string, StoredGroupName>();
  if (typeof obj.g === "object" && obj.g !== null) {
    for (const [groupIdB64, raw] of Object.entries(obj.g)) {
      if (typeof raw !== "object" || raw === null) continue;
      const n = (raw as { n?: unknown }).n;
      const t = (raw as { t?: unknown }).t;
      if (typeof n !== "string") continue;
      if (typeof t !== "string" || !/^\d+$/.test(t)) continue;
      rows.set(groupIdB64, { name: n, updatedAtMs: BigInt(t) });
    }
  }
  return rows;
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/** Where the rows live between sessions. One row per account. */
export interface GroupNameBackend {
  load(accountId: string): Promise<ReadonlyMap<string, StoredGroupName> | null>;
  save(accountId: string, rows: ReadonlyMap<string, StoredGroupName>): Promise<void>;
}

/** A backend that keeps nothing (tests, a device with no key). */
export const MEMORY_GROUP_NAME_BACKEND: GroupNameBackend = {
  load: async () => null,
  save: async () => undefined,
};

/**
 * The in-memory view of one account's shared group names, written through
 * to the backend. Decisions are the caller's ({@link groupNameVerdict});
 * this only stores what it is told to.
 */
export class GroupNameStore {
  private rows = new Map<string, StoredGroupName>();
  private account: string | null = null;
  private readonly listeners = new Set<() => void>();
  private chain: Promise<void> = Promise.resolve();

  constructor(
    private readonly backend: GroupNameBackend,
    private readonly onPersistError: (err: unknown) => void = () => undefined,
  ) {}

  /** Load the account's row. A row this device cannot read starts empty.
   *  Resolves `false` when the backend THREW — the caller then cannot tell
   *  "nothing stored" from "unreadable", which the verdict treats as
   *  unverifiable (fail-closed) rather than as a first observation. */
  async open(accountId: string): Promise<boolean> {
    this.account = accountId;
    let loaded: ReadonlyMap<string, StoredGroupName> | null = null;
    let readable = true;
    try {
      loaded = await this.backend.load(accountId);
    } catch (err) {
      readable = false;
      this.onPersistError(err);
    }
    // The session may have moved on while the load was in flight.
    if (this.account !== accountId) return readable;
    const next = new Map(loaded ?? []);
    // A write that landed before the load settled wins over the stored row.
    for (const [k, v] of this.rows) next.set(k, v);
    this.rows = next;
    this.notify();
    return readable;
  }

  /** Forget everything in memory (sign-out). The row on disk stays. */
  close(): void {
    this.account = null;
    this.rows = new Map();
    this.notify();
  }

  /** The stored value for a group, stamp included. */
  get(groupIdB64: string): StoredGroupName | undefined {
    return this.rows.get(groupIdB64);
  }

  /** The shared name to SHOW, or `null` when none / cleared. */
  nameOf(groupIdB64: string): string | null {
    const v = this.rows.get(groupIdB64);
    return v === undefined || v.name.length === 0 ? null : v.name;
  }

  /** Every stored `(groupIdB64, value)`. */
  entries(): IterableIterator<[string, StoredGroupName]> {
    return this.rows.entries();
  }

  /** Replace the value for a group and persist. The caller has already
   *  decided (verdict, or its own rename). Resolves once the write settled;
   *  a persist failure is reported and swallowed — the in-memory value
   *  stands for the session either way. */
  set(accountId: string, groupIdB64: string, value: StoredGroupName): Promise<void> {
    this.rows = new Map(this.rows).set(groupIdB64, value);
    this.notify();
    if (this.account !== accountId) return Promise.resolve();
    const snapshot: ReadonlyMap<string, StoredGroupName> = this.rows;
    this.chain = this.chain
      .then(() => this.backend.save(accountId, snapshot))
      .catch((err: unknown) => {
        this.onPersistError(err);
      });
    return this.chain;
  }

  /** Called on any change. */
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
