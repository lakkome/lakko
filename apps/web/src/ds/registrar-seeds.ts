// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 obligation 4 - the registrar seed a group's owner hands this
 * account, received, gated, and kept.
 *
 * The owner appoints each member a registrar (`ownership.ts`) and sends the
 * member's seed inside the group as an MLS application message: a
 * `lakko-contacts` envelope sealed to the member's ACCOUNT X25519 key, with a
 * clear header naming the journal, the addressee's account key and the rank.
 *
 * ## The receive gate
 *
 * Every device of the addressee account keeps it: the seed is sealed to the
 * ACCOUNT key, which every device of the account holds, so each device that
 * reads the envelope opens and stores it, whichever device read it first
 * (ADR-0057, amended 2026-10-01, item 2). A device that joined after the
 * envelope was posted gets its own copy: the owner's delivery record is kept
 * per device leaf (`appointments.ts`).
 *
 * Kept only when ALL of these hold, else dropped without a trace in the
 * timeline (it is never a chat bubble):
 *
 *   1. it decodes (the wasm codec is strict: domain, version, exact length);
 *   2. it names THIS journal (a seed for another group is noise here);
 *   3. it is addressed to THIS account's key;
 *   4. its MLS sender resolves, AT RECEIPT (C-68), to the group's OWNER
 *      through the live role table. A member cannot hand another member a
 *      seed of its own making: the rank would then mean nothing.
 *
 * Then `openRegistrarSeedEnvelope(plaintext, accountIdentity)` opens the seed
 * (it refuses once more if the box does not open under this account's key),
 * and a newer envelope replaces an older one for the same group.
 *
 * ## Where the seed lives
 *
 * Per (account, group), with the protection every other per-account secret
 * has on web: an IndexedDB row encrypted under the device root key
 * (AES-GCM, `getDeviceKey`), mirrored in memory for the synchronous reads the
 * signer paths need. Never logged, never synced, erased with the device.
 */

/** A kept seed and the rank the envelope named. */
export interface StoredRegistrarSeed {
  readonly seed: Uint8Array;
  /** `1` member, `2` admin. */
  readonly rank: number;
}

/** The decoded clear header of a registrar-seed envelope. */
export interface RegistrarSeedHeader {
  readonly journalId: Uint8Array;
  readonly memberAccountKey: Uint8Array;
  readonly rank: number;
  /** The registrar's public key from the clear header (envelope v2). Read for
   *  the registrar list (`registrar-list.ts`); the open checks the seed
   *  derives it. */
  readonly registrarPublicKey?: Uint8Array;
}

/** Why an envelope was dropped, or `accept`. */
export type RegistrarSeedVerdict =
  | "accept"
  | "malformed"
  | "wrong-journal"
  | "not-addressed"
  | "sender-not-owner";

function sameBytes(a: Uint8Array | undefined, b: Uint8Array | undefined): boolean {
  if (a === undefined || b === undefined || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** The pure receive gate (rules 1-4 above). `header` is `null` when the
 *  envelope did not decode. `ownerAccountKey` is the live role table's owner
 *  read at receipt, `undefined` for a group without one (no owner, so no
 *  seed is ever kept there). */
export function registrarSeedVerdict(args: {
  readonly header: RegistrarSeedHeader | null;
  readonly groupId: Uint8Array;
  readonly ownAccountKey: Uint8Array;
  readonly senderAccountKey: Uint8Array | undefined;
  readonly ownerAccountKey: Uint8Array | undefined;
}): RegistrarSeedVerdict {
  const { header } = args;
  if (header === null) return "malformed";
  if (header.rank !== 1 && header.rank !== 2) return "malformed";
  if (!sameBytes(header.journalId, args.groupId)) return "wrong-journal";
  if (!sameBytes(header.memberAccountKey, args.ownAccountKey)) return "not-addressed";
  if (!sameBytes(args.senderAccountKey, args.ownerAccountKey)) return "sender-not-owner";
  return "accept";
}

/** What the receive side needs from the outside world. */
export interface RegistrarSeedReceivePorts {
  /** `decodeRegistrarSeedEnvelope`, `null` on any refusal. */
  decode(plaintext: Uint8Array): RegistrarSeedHeader | null;
  /** `openRegistrarSeedEnvelope(plaintext, accountIdentity)`; throws when it
   *  does not open. */
  open(plaintext: Uint8Array): Uint8Array;
  /** This account's key (the account identity's signature key), or `null`
   *  when no account is signed in. */
  ownAccountKey(): Uint8Array | null;
  /** Keep the seed for the group (replacing an older one). */
  keep(groupIdB64: string, seed: StoredRegistrarSeed): Promise<void>;
}

/**
 * Run the gate on one received envelope and keep the seed when it passes.
 * Returns the verdict (`accept` only when the seed was opened and kept).
 * Never throws: every failure is a drop.
 */
export async function receiveRegistrarSeed(
  ports: RegistrarSeedReceivePorts,
  args: {
    readonly groupId: Uint8Array;
    readonly groupIdB64: string;
    readonly plaintext: Uint8Array;
    readonly senderAccountKey: Uint8Array | undefined;
    readonly ownerAccountKey: Uint8Array | undefined;
  },
): Promise<RegistrarSeedVerdict> {
  const ownAccountKey = ports.ownAccountKey();
  if (ownAccountKey === null) return "not-addressed";
  let header: RegistrarSeedHeader | null;
  try {
    header = ports.decode(args.plaintext);
  } catch {
    header = null;
  }
  const verdict = registrarSeedVerdict({
    header,
    groupId: args.groupId,
    ownAccountKey,
    senderAccountKey: args.senderAccountKey,
    ownerAccountKey: args.ownerAccountKey,
  });
  if (verdict !== "accept" || header === null) return verdict;
  let seed: Uint8Array;
  try {
    seed = ports.open(args.plaintext);
  } catch {
    return "malformed";
  }
  if (seed.length !== 32) return "malformed";
  try {
    await ports.keep(args.groupIdB64, { seed, rank: header.rank });
  } catch (err) {
    // The seed is not logged; the failure is.
    // eslint-disable-next-line no-console
    console.warn("ds: registrar seed could not be stored", err instanceof Error ? err.name : "error");
  }
  return "accept";
}

// ----------------------------------------------------------------------------
// The per-account store
// ----------------------------------------------------------------------------

/** Where the rows go: one encrypted row per account. */
export interface RegistrarSeedBackend {
  load(accountId: string): Promise<ReadonlyMap<string, StoredRegistrarSeed> | null>;
  save(accountId: string, rows: ReadonlyMap<string, StoredRegistrarSeed>): Promise<void>;
}

/** Encode the rows as the JSON the backend encrypts. */
export function encodeRegistrarSeedRows(rows: ReadonlyMap<string, StoredRegistrarSeed>): string {
  const out: Record<string, { s: string; r: number }> = {};
  for (const [group, v] of rows) {
    let bin = "";
    for (let i = 0; i < v.seed.length; i++) bin += String.fromCharCode(v.seed[i] ?? 0);
    out[group] = { s: btoa(bin), r: v.rank };
  }
  return JSON.stringify(out);
}

/** Decode the JSON; a malformed row is skipped rather than failing the rest. */
export function decodeRegistrarSeedRows(json: string): Map<string, StoredRegistrarSeed> {
  const out = new Map<string, StoredRegistrarSeed>();
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return out;
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return out;
  for (const [group, raw] of Object.entries(parsed as Record<string, unknown>)) {
    if (raw === null || typeof raw !== "object") continue;
    const { s, r } = raw as { s?: unknown; r?: unknown };
    if (typeof s !== "string" || (r !== 1 && r !== 2)) continue;
    let seed: Uint8Array;
    try {
      const bin = atob(s);
      seed = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) seed[i] = bin.charCodeAt(i);
    } catch {
      continue;
    }
    if (seed.length !== 32) continue;
    out.set(group, { seed, rank: r });
  }
  return out;
}

/**
 * The in-memory mirror the signer paths read synchronously, written through
 * to the backend. One account at a time.
 */
export class RegistrarSeedStore {
  private account: string | null = null;
  private rows = new Map<string, StoredRegistrarSeed>();
  private backend: RegistrarSeedBackend | null = null;

  /** Bind to an account and load its rows. A failed load starts empty. */
  async open(accountId: string, backend: RegistrarSeedBackend): Promise<void> {
    this.account = accountId;
    this.backend = backend;
    this.rows = new Map();
    try {
      const loaded = await backend.load(accountId);
      if (this.account !== accountId) return;
      if (loaded !== null) {
        // Rows kept while the load was in flight win.
        const merged = new Map(loaded);
        for (const [k, v] of this.rows) merged.set(k, v);
        this.rows = merged;
      }
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ds: registrar seeds could not be loaded", err instanceof Error ? err.name : "error");
    }
  }

  /** Forget the account (sign-out). */
  close(): void {
    this.account = null;
    this.backend = null;
    this.rows = new Map();
  }

  get(groupIdB64: string): StoredRegistrarSeed | undefined {
    return this.rows.get(groupIdB64);
  }

  /** Drop the seed for a group (ADR-0057 root handover: this account became
   *  root, and the old root's registrars are removed), then persist. */
  async delete(groupIdB64: string): Promise<void> {
    const account = this.account;
    const backend = this.backend;
    if (account === null) return;
    const prior = this.rows.get(groupIdB64);
    if (prior === undefined) return;
    prior.seed.fill(0);
    this.rows.delete(groupIdB64);
    if (backend !== null) await backend.save(account, this.rows);
  }

  /** Keep (or replace) the seed for a group, then persist. */
  async set(groupIdB64: string, value: StoredRegistrarSeed): Promise<void> {
    const account = this.account;
    const backend = this.backend;
    if (account === null) return;
    this.rows.set(groupIdB64, { seed: value.seed.slice(), rank: value.rank });
    if (backend !== null) await backend.save(account, this.rows);
  }
}

/** The app's store; `ChatContext` binds it to the signed-in account. */
export const registrarSeeds = new RegistrarSeedStore();

// ----------------------------------------------------------------------------
// The sink the chat receive path hands envelopes to
// ----------------------------------------------------------------------------

/** What the chat receive path reports for one envelope it recognised. */
export interface RegistrarSeedArrival {
  readonly groupId: Uint8Array;
  readonly groupIdB64: string;
  readonly plaintext: Uint8Array;
  /** The MLS sender's account key, resolved at receipt (C-68). */
  readonly senderAccountKey: Uint8Array | undefined;
  /** The live role table's owner, read at receipt. */
  readonly ownerAccountKey: Uint8Array | undefined;
}

/** What the handler reports for one arrival: the registrar-seed verdict, or
 *  for a rank-root envelope the root-handover one (`root-handover.ts`). */
export type SeedArrivalVerdict =
  | RegistrarSeedVerdict
  | "not-owner";

type Handler = (arrival: RegistrarSeedArrival) => Promise<SeedArrivalVerdict>;

let handler: Handler | null = null;

/** Install the handler (ChatContext does, once an account is signed in);
 *  returns the uninstaller. */
export function installRegistrarSeedHandler(h: Handler): () => void {
  handler = h;
  return () => {
    if (handler === h) handler = null;
  };
}

/** Hand one envelope over. With no handler installed (signed out, a test
 *  that is not about this) it is dropped. Never throws. */
export async function deliverRegistrarSeed(arrival: RegistrarSeedArrival): Promise<SeedArrivalVerdict> {
  if (handler === null) return "not-addressed";
  try {
    return await handler(arrival);
  } catch {
    return "malformed";
  }
}
