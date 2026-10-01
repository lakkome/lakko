// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 "What the S2 client work found", item 1 - an ownership transfer
 * moves the journal root.
 *
 * ADR-0045 ownership transfer rewrites the role table, not the journal: the
 * Delivery Service still knows the OLD owner's account-derived root, and the
 * old owner cannot name the new owner's root (that key is derived from the
 * new owner's account seed). So the old owner hands its root seed over:
 *
 *   - **Sending (outgoing owner).** When a device of this account applies a
 *     remote commit or merges its own sweep, and the role table's owner was
 *     this account BEFORE and another account AFTER, the device seals
 *     `dsJournalRootSeed(ownAccountSeed, gid)` to the new owner's account
 *     X25519 key (`composeRootHandoverEnvelope`, the registrar-seed envelope
 *     at rank root) and posts it in the group. It stops acting as root the
 *     moment the role table changes: the journal authority is resolved off
 *     the live role table (`ds/access.ts` owner source), so no appointment,
 *     root registration, claim or reset is signed with the root after that.
 *     Every device of the outgoing account may send it. The record of the
 *     owed handover is kept until the outgoing account's own probe (a `root`
 *     statement naming its own root, signed by it) answers 401, i.e. the DS
 *     no longer knows it as the root; while the probe answers 204 the
 *     envelope is re-sent, at most every {@link HANDOVER_RESEND_MS}, on
 *     session start and on group activity (the second security pass,
 *     finding 4: a one-shot send lost to a closed tab left the old root in
 *     place for good).
 *   - **Receiving (new owner).** A rank-root envelope is kept only when it
 *     names this journal, is addressed to this account and the live role
 *     table AT RECEIPT names this account owner (C-68). There is no sender
 *     check: the DS is the check, because a seed that is not the current root
 *     fails the `root` statement with 401. The opened seed signs `root`
 *     naming this account's own derived root, then (after a 204) the new root
 *     sends `registrars-set` with the whole set the live roster calls for
 *     (every old registrar not listed is removed in the same statement) and
 *     the normal appointment reconcile re-sends every member's seed under the
 *     new root. Until the 204 (or the probe) confirms it, this account keeps
 *     signing with its registrar seed (`root-held.ts`). The opened seed is
 *     kept wrapped under the device key (`PendingRootSeedStore`) until the
 *     takeover succeeds or answers 401, so a network failure or a closed tab
 *     retries it; it never goes into the registrar-seed store and is never
 *     logged.
 *
 * The takeover runs DETACHED from the receive loop: the loop holds the
 * group's MLS lock, and the appointment reconcile it ends with publishes
 * into the group under that same lock.
 *
 * Everything here takes its signers, the wasm codec and the network through
 * ports so the host tests drive the rules with fakes.
 */
import {
  isStatus,
  releaseAuthority,
  sendStatement,
  type JournalAuthority,
  type RegistrarDerivation,
  type StatementRequestLike,
  type StatementSender,
} from "./ownership";
import { sendRegistrarSet } from "./appointments";
import type { RegistrarSeedHeader } from "./registrar-seeds";

/** The outgoing owner re-sends an owed handover at most this often while its
 *  probe says it is still the root. Chosen, not measured. */
export const HANDOVER_RESEND_MS = 10 * 60_000;

/** The registrar-seed envelope's rank byte for a root handover
 *  (`lakko_ds_proof::ROOT_RANK_BYTE`). */
export const RANK_ROOT_HANDOVER = 3;

function sameBytes(a: Uint8Array | undefined, b: Uint8Array | undefined): boolean {
  if (a === undefined || b === undefined || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] ?? 0);
  return btoa(s);
}

function wipe(bytes: Uint8Array | null | undefined): void {
  if (bytes === null || bytes === undefined) return;
  try {
    bytes.fill(0);
  } catch {
    /* a detached buffer holds nothing to wipe */
  }
}

// ----------------------------------------------------------------------------
// Detecting an owner change at commit apply / sweep merge
// ----------------------------------------------------------------------------

/** The role table's owner, `undefined` for a table-less group or a torn read. */
export function readRoleTableOwner(group: { roleTableOwner?: () => Uint8Array | undefined }): Uint8Array | undefined {
  try {
    const owner = group.roleTableOwner?.();
    return owner instanceof Uint8Array && owner.length === 32 ? owner.slice() : undefined;
  } catch {
    return undefined;
  }
}

/**
 * The pure sending rule: the new owner when the role table's owner was
 * `own` before and is ANOTHER account after, else `null` (a no-op commit,
 * this account staying owner, becoming owner, or an unreadable table).
 */
export function ownerMovedAwayFrom(
  own: Uint8Array | null | undefined,
  before: Uint8Array | undefined,
  after: Uint8Array | undefined,
): Uint8Array | null {
  if (own === null || own === undefined) return null;
  if (!sameBytes(before, own)) return null;
  if (after === undefined || sameBytes(after, own)) return null;
  return after;
}

/** One owner change the apply paths observed. */
export interface OwnerTransition {
  readonly groupId: Uint8Array;
  readonly groupIdB64: string;
  readonly before: Uint8Array | undefined;
  readonly after: Uint8Array | undefined;
}

type TransitionHandler = (t: OwnerTransition) => void;
let transitionHandler: TransitionHandler | null = null;

/** Install the handler (the journal-ownership provider does); returns the
 *  uninstaller. */
export function installOwnerTransitionHandler(h: TransitionHandler): () => void {
  transitionHandler = h;
  return () => {
    if (transitionHandler === h) transitionHandler = null;
  };
}

/**
 * Report the role-table owner read before and after one commit apply or
 * sweep merge. Called from inside the group's MLS lock, so the handler only
 * schedules; it never awaits the network. No-op when unchanged. Never throws.
 */
export function noteOwnerTransition(
  groupId: Uint8Array,
  before: Uint8Array | undefined,
  after: Uint8Array | undefined,
): void {
  if (before === undefined && after === undefined) return;
  if (sameBytes(before, after)) return;
  const h = transitionHandler;
  if (h === null) return;
  try {
    h({ groupId, groupIdB64: b64(groupId), before, after });
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("ds: owner transition handler failed", err instanceof Error ? err.name : "error");
  }
}

// ----------------------------------------------------------------------------
// Sending (outgoing owner)
// ----------------------------------------------------------------------------

export interface HandoverSendPorts {
  readonly groupId: Uint8Array;
  /** This account's key, or `null` when signed out. */
  ownAccountKey(): Uint8Array | null;
  /** The live role table's owner now. */
  liveOwner(): Uint8Array | undefined;
  /** The new owner's account X25519 key from its identity bundle (verified
   *  to belong to that account key), or `null` when not resolvable yet. */
  ownerX25519(accountKeyB64: string): Promise<Uint8Array | null>;
  /** `dsJournalRootSeed(ownAccountSeed, groupId)`; the caller wipes it. */
  rootSeed(): Uint8Array;
  /** `composeRootHandoverEnvelope(groupId, newOwner, x25519, rootSeed)`. */
  compose(newOwnerAccountKey: Uint8Array, newOwnerX25519: Uint8Array, rootSeed: Uint8Array): Uint8Array;
  /** Seal and publish into the group (an MLS application message). */
  sendEnvelope(envelope: Uint8Array): Promise<void>;
}

export type HandoverSendOutcome =
  /** Posted in the group. */
  | "sent"
  /** Nothing to hand over any more: this account is owner again, the group
   *  has no owner, or the session ended. */
  | "cancelled"
  /** The new owner's X25519 key is not resolvable yet: retried later. */
  | "pending"
  /** The send failed: retried later. */
  | "failed";

/**
 * One handover attempt. The recipient is the owner the LIVE role table names
 * now (not the one recorded when the change was seen), so a second transfer
 * in between hands the root to whoever holds the group. Never sent while
 * this account is owner.
 */
export async function sendRootHandover(ports: HandoverSendPorts): Promise<HandoverSendOutcome> {
  const own = ports.ownAccountKey();
  if (own === null) return "cancelled";
  const owner = ports.liveOwner();
  if (owner === undefined || sameBytes(owner, own)) return "cancelled";
  let x25519: Uint8Array | null;
  try {
    x25519 = await ports.ownerX25519(b64(owner));
  } catch {
    x25519 = null;
  }
  if (x25519 === null) return "pending";
  // Re-read after the await: the table may have moved while the bundle was
  // fetched.
  const ownerNow = ports.liveOwner();
  if (ownerNow === undefined || sameBytes(ownerNow, own)) return "cancelled";
  if (!sameBytes(ownerNow, owner)) return "pending";
  let envelope: Uint8Array;
  const rootSeed = ports.rootSeed();
  try {
    envelope = ports.compose(owner, x25519, rootSeed);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("ds: root handover envelope not composed", err instanceof Error ? err.name : "error");
    return "failed";
  } finally {
    wipe(rootSeed);
  }
  try {
    await ports.sendEnvelope(envelope);
    return "sent";
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("ds: root handover envelope not sent; retried later", err instanceof Error ? err.name : "error");
    return "failed";
  }
}

/** What the outgoing owner's owed-handover step needs beyond the send. */
export interface OwedHandoverPorts extends HandoverSendPorts {
  /** The probe with this account's own root: `root` on 204, `not-root` on
   *  401; throws on anything else. */
  probeOwnRoot(): Promise<"root" | "not-root">;
  /** When this session last sent the envelope for the group, or `null`. */
  lastSentAt(): number | null;
  noteSent(at: number): void;
  now(): number;
}

export type OwedHandoverOutcome =
  /** The probe answered 401: the root moved; the owed record goes. */
  | "done"
  /** Sent this round; kept until the probe answers 401. */
  | "sent"
  /** Still the root, but sent less than {@link HANDOVER_RESEND_MS} ago. */
  | "throttled"
  /** Nothing to hand over (this account is owner again, or the group has no
   *  owner): the owed record goes. */
  | "cancelled"
  /** The probe or the send could not run (offline, 5xx, no box key yet):
   *  retried later. */
  | "retry";

/**
 * One step of an owed handover: probe; while this account is still the root,
 * (re-)send the envelope unless it went out less than
 * {@link HANDOVER_RESEND_MS} ago. Never throws.
 */
export async function runOwedHandover(ports: OwedHandoverPorts): Promise<OwedHandoverOutcome> {
  let probe: "root" | "not-root";
  try {
    probe = await ports.probeOwnRoot();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.info("ds: root probe failed; the handover is retried later", err instanceof Error ? err.name : "error");
    return "retry";
  }
  if (probe === "not-root") {
    // eslint-disable-next-line no-console
    console.info("ds: root probe answered 401; the handover landed");
    return "done";
  }
  const last = ports.lastSentAt();
  if (last !== null && ports.now() - last < HANDOVER_RESEND_MS) return "throttled";
  const outcome = await sendRootHandover(ports);
  if (outcome === "sent") {
    ports.noteSent(ports.now());
    return "sent";
  }
  if (outcome === "cancelled") return "cancelled";
  return "retry";
}

// ----------------------------------------------------------------------------
// The owed record: which groups this account still owes a handover for
// ----------------------------------------------------------------------------

const OWED_PREFIX = "lakko/ds-root-handover/v1/";

/** The storage the owed record uses; injectable for tests. Group ids only -
 *  nothing secret. */
export interface OwedStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

function defaultStorage(): OwedStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** The groups (base64 ids) this account still owes a handover for. */
export function loadOwedHandovers(accountId: string, storage: OwedStorage | null = defaultStorage()): Set<string> {
  const out = new Set<string>();
  if (storage === null) return out;
  try {
    const raw = storage.getItem(OWED_PREFIX + accountId);
    if (raw === null) return out;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return out;
    for (const v of parsed) if (typeof v === "string") out.add(v);
  } catch {
    /* unreadable: nothing owed; another device of the account may still send */
  }
  return out;
}

function saveOwed(accountId: string, owed: ReadonlySet<string>, storage: OwedStorage | null): void {
  if (storage === null) return;
  try {
    if (owed.size === 0) {
      storage.removeItem?.(OWED_PREFIX + accountId);
      if (storage.removeItem === undefined) storage.setItem(OWED_PREFIX + accountId, "[]");
    } else {
      storage.setItem(OWED_PREFIX + accountId, JSON.stringify([...owed].sort()));
    }
  } catch {
    /* best effort: a lost record costs this device's handover, not another's */
  }
}

export function markHandoverOwed(accountId: string, groupIdB64: string, storage: OwedStorage | null = defaultStorage()): void {
  const owed = loadOwedHandovers(accountId, storage);
  owed.add(groupIdB64);
  saveOwed(accountId, owed, storage);
}

export function clearHandoverOwed(accountId: string, groupIdB64: string, storage: OwedStorage | null = defaultStorage()): void {
  const owed = loadOwedHandovers(accountId, storage);
  if (!owed.delete(groupIdB64)) return;
  saveOwed(accountId, owed, storage);
}

/** Forget one account's record (forget device). */
export function clearOwedHandovers(accountId: string, storage: OwedStorage | null = defaultStorage()): void {
  if (storage === null) return;
  try {
    storage.removeItem?.(OWED_PREFIX + accountId);
  } catch {
    /* best effort */
  }
}

// ----------------------------------------------------------------------------
// Receiving (new owner)
// ----------------------------------------------------------------------------

/** Why a rank-root envelope was dropped, or `accept`. */
export type RootHandoverVerdict = "accept" | "malformed" | "wrong-journal" | "not-addressed" | "not-owner";

/** The pure receive gate. `ownerAccountKey` is the live role table's owner
 *  read at receipt. No sender check: the DS refuses a seed that is not the
 *  current root. */
export function rootHandoverVerdict(args: {
  readonly header: RegistrarSeedHeader | null;
  readonly groupId: Uint8Array;
  readonly ownAccountKey: Uint8Array;
  readonly ownerAccountKey: Uint8Array | undefined;
}): RootHandoverVerdict {
  const { header } = args;
  if (header === null || header.rank !== RANK_ROOT_HANDOVER) return "malformed";
  if (!sameBytes(header.journalId, args.groupId)) return "wrong-journal";
  if (!sameBytes(header.memberAccountKey, args.ownAccountKey)) return "not-addressed";
  if (!sameBytes(args.ownerAccountKey, args.ownAccountKey)) return "not-owner";
  return "accept";
}

export interface RootHandoverReceivePorts {
  /** `openRegistrarSeedEnvelope(plaintext, accountIdentity)`; throws when it
   *  does not open. */
  open(plaintext: Uint8Array): Uint8Array;
  /** This account's key, or `null` when signed out. */
  ownAccountKey(): Uint8Array | null;
  /** Start the takeover, detached; it owns (and wipes) `oldRootSeed`. */
  takeOver(groupId: Uint8Array, groupIdB64: string, oldRootSeed: Uint8Array): void;
}

/**
 * Run the gate on one rank-root envelope whose clear header was decoded to
 * `header`, and start the takeover when it passes. Never throws.
 */
export function receiveRootHandover(
  ports: RootHandoverReceivePorts,
  args: {
    readonly header: RegistrarSeedHeader | null;
    readonly groupId: Uint8Array;
    readonly groupIdB64: string;
    readonly plaintext: Uint8Array;
    readonly ownerAccountKey: Uint8Array | undefined;
  },
): RootHandoverVerdict {
  const ownAccountKey = ports.ownAccountKey();
  if (ownAccountKey === null) return "not-addressed";
  const verdict = rootHandoverVerdict({
    header: args.header,
    groupId: args.groupId,
    ownAccountKey,
    ownerAccountKey: args.ownerAccountKey,
  });
  if (verdict !== "accept") return verdict;
  let seed: Uint8Array;
  try {
    seed = ports.open(args.plaintext);
  } catch {
    return "malformed";
  }
  if (seed.length !== 32) {
    wipe(seed);
    return "malformed";
  }
  try {
    ports.takeOver(args.groupId, args.groupIdB64, seed);
  } catch {
    wipe(seed);
  }
  return "accept";
}

/** The old root's signer, built from the handed-over seed. */
export interface RootTransferSignerLike {
  rootTransfer(journalId: Uint8Array, newRootPublicKey: Uint8Array): StatementRequestLike;
  free(): void;
}

export interface TakeOverPorts {
  readonly groupId: Uint8Array;
  readonly sender: StatementSender;
  /** `dsStatementSigner(seed)`. */
  signerOf(seed: Uint8Array): RootTransferSignerLike;
  /** `dsJournalRootSigner(ownAccountIdentity, groupId).publicKey`. */
  ownRootPublicKey(): Uint8Array;
  /** The live role table still names this account owner. */
  stillOwner(): boolean;
  /** The registrar set the live roster and role table call for (base64
   *  account key -> rank, never this account), or `null` when unreadable. */
  desiredRegistrars(): ReadonlyMap<string, number> | null;
  readonly derive: RegistrarDerivation;
  /** Set this account's "root held" marker (`root-held.ts`). */
  markRootHeld(): void;
  /** The probe with this account's own root, after a 401: another device of
   *  this account may have taken over already. */
  probeOwnRoot(): Promise<"root" | "not-root">;
  /** This device's authority after the takeover: the new root. */
  authority(): JournalAuthority;
  /** Forget which seeds were delivered (every seed changes with the root)
   *  and drop this account's own old registrar seed for the group. */
  resetAppointments(): Promise<void>;
  /** Schedule the normal appointment reconcile under the new root. */
  reconcile(): void;
  /** Delays between attempts on a transient failure (network, 5xx). */
  readonly retryDelaysMs?: readonly number[];
  sleep?(ms: number): Promise<void>;
}

export type TakeOverOutcome =
  /** The root moved to this account's root (204), the registrar set reset
   *  (or its reset given up on), reconcile scheduled. */
  | "took-over"
  /** 401, and this account's own probe says it holds the root: another
   *  device of this account took over already. Marker set; seed dropped. */
  | "held-elsewhere"
  /** 401: the seed is not the current root (the envelope is stale or
   *  forged). Dropped. */
  | "stale"
  /** The role table no longer names this account owner. */
  | "not-owner"
  /** The root statement kept failing transiently. */
  | "failed";

/** Chosen, not measured: a short retry, then one after a poll interval. */
const DEFAULT_RETRY_DELAYS_MS = [5_000, 30_000];

function isTransient(err: unknown): boolean {
  if (err === null || typeof err !== "object") return true;
  const status = (err as { status?: unknown }).status;
  if (typeof status !== "number") return true;
  return status >= 500;
}

async function withTransientRetry(
  ports: TakeOverPorts,
  run: () => Promise<void>,
): Promise<void> {
  const delays = ports.retryDelaysMs ?? DEFAULT_RETRY_DELAYS_MS;
  const sleep = ports.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  for (let attempt = 0; ; attempt++) {
    try {
      await run();
      return;
    } catch (err) {
      const delay = delays[attempt];
      if (!isTransient(err) || delay === undefined) throw err;
      await sleep(delay);
    }
  }
}

/**
 * The new owner's takeover with the old root seed: `root` naming this
 * account's derived root, then removal of the old root's registrars, then a
 * fresh appointment reconcile. Wipes the seed on every path. Never throws.
 */
export async function takeOverJournal(ports: TakeOverPorts, oldRootSeed: Uint8Array): Promise<TakeOverOutcome> {
  try {
    if (!ports.stillOwner()) return "not-owner";

    // 1. The root statement, signed by the OLD root, naming this account's.
    let newRoot: Uint8Array;
    try {
      newRoot = ports.ownRootPublicKey();
    } catch {
      return "failed";
    }
    const old = ports.signerOf(oldRootSeed);
    try {
      await withTransientRetry(ports, () =>
        sendStatement(ports.sender, () => old.rootTransfer(ports.groupId, newRoot)),
      );
    } catch (err) {
      if (isStatus(err, 401)) {
        let own: "root" | "not-root" | "unknown" = "unknown";
        try {
          own = await ports.probeOwnRoot();
        } catch {
          own = "unknown";
        }
        if (own === "root") {
          ports.markRootHeld();
          // eslint-disable-next-line no-console
          console.info("ds: root handover already applied by another device of this account");
          ports.reconcile();
          return "held-elsewhere";
        }
        // eslint-disable-next-line no-console
        console.info("ds: root handover not applied (401): the seed is not the current root; dropped");
        return "stale";
      }
      // eslint-disable-next-line no-console
      console.warn("ds: root handover failed", err instanceof Error ? err.name : "error");
      return "failed";
    } finally {
      try {
        old.free();
      } catch {
        /* already freed */
      }
    }

    // The root is this account's from here: sign with it (not the registrar
    // seed) from now on.
    ports.markRootHeld();

    // 2. One `registrars-set` under the new root: the whole set the roster
    //    calls for, every old root's registrar not listed removed with it.
    const authority = ports.authority();
    try {
      const desired = ports.desiredRegistrars();
      if (authority.kind === "root" && desired !== null) {
        await withTransientRetry(ports, () =>
          sendRegistrarSet(ports.sender, ports.groupId, authority, desired, ports.derive),
        );
      }
    } catch (err) {
      // The root moved; the reconcile below sends the set again. Logged, not
      // fatal.
      // eslint-disable-next-line no-console
      console.warn("ds: registrar set not reset after root handover", err instanceof Error ? err.name : "error");
    } finally {
      releaseAuthority(authority);
    }

    // 3. Appoint every member again under the new root.
    try {
      await ports.resetAppointments();
    } catch {
      /* the reconcile still runs; a stale record costs a missed appointment
         until the roster next changes */
    }
    ports.reconcile();
    return "took-over";
  } finally {
    wipe(oldRootSeed);
  }
}

// ----------------------------------------------------------------------------
// The received old root seed, kept until the takeover lands
// ----------------------------------------------------------------------------

/** Where the pending seeds go: one encrypted row per account. */
export interface PendingRootSeedBackend {
  load(accountId: string): Promise<ReadonlyMap<string, Uint8Array> | null>;
  save(accountId: string, rows: ReadonlyMap<string, Uint8Array>): Promise<void>;
}

/** Encode the rows (group id -> 32-byte seed) as the JSON the backend
 *  encrypts. */
export function encodePendingRootSeedRows(rows: ReadonlyMap<string, Uint8Array>): string {
  const out: Record<string, string> = {};
  for (const [group, seed] of rows) out[group] = b64(seed);
  return JSON.stringify(out);
}

/** Decode the JSON; a malformed row is skipped. */
export function decodePendingRootSeedRows(json: string): Map<string, Uint8Array> {
  const out = new Map<string, Uint8Array>();
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return out;
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return out;
  for (const [group, raw] of Object.entries(parsed as Record<string, unknown>)) {
    if (typeof raw !== "string") continue;
    try {
      const bin = atob(raw);
      const seed = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) seed[i] = bin.charCodeAt(i);
      if (seed.length === 32) out.set(group, seed);
    } catch {
      /* skipped */
    }
  }
  return out;
}

/**
 * The old root seeds this account received as the new owner and has not yet
 * used successfully, per group: an in-memory mirror written through to the
 * backend (encrypted under the device key, like every other per-account
 * secret; never logged, never synced). One account at a time.
 */
export class PendingRootSeedStore {
  private account: string | null = null;
  private rows = new Map<string, Uint8Array>();
  private backend: PendingRootSeedBackend | null = null;

  /** Bind to an account and load its rows. A failed load starts empty. */
  async open(accountId: string, backend: PendingRootSeedBackend): Promise<void> {
    this.account = accountId;
    this.backend = backend;
    this.rows = new Map();
    try {
      const loaded = await backend.load(accountId);
      if (this.account !== accountId || loaded === null) return;
      const merged = new Map(loaded);
      for (const [k, v] of this.rows) merged.set(k, v);
      this.rows = merged;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("ds: pending root seeds could not be loaded", err instanceof Error ? err.name : "error");
    }
  }

  /** Forget the account (sign-out); wipes the mirror. */
  close(): void {
    for (const seed of this.rows.values()) wipe(seed);
    this.account = null;
    this.backend = null;
    this.rows = new Map();
  }

  /** The groups with a pending seed. */
  groups(): string[] {
    return [...this.rows.keys()];
  }

  /** A COPY of the pending seed for a group (the caller wipes it). */
  get(groupIdB64: string): Uint8Array | undefined {
    return this.rows.get(groupIdB64)?.slice();
  }

  /** Keep (or replace) the seed for a group, then persist. */
  async set(groupIdB64: string, seed: Uint8Array): Promise<void> {
    const account = this.account;
    if (account === null) return;
    const prior = this.rows.get(groupIdB64);
    if (prior !== undefined) wipe(prior);
    this.rows.set(groupIdB64, seed.slice());
    if (this.backend !== null) await this.backend.save(account, this.rows);
  }

  /** Wipe and drop the seed for a group, then persist. */
  async delete(groupIdB64: string): Promise<void> {
    const account = this.account;
    if (account === null) return;
    const prior = this.rows.get(groupIdB64);
    if (prior === undefined) return;
    wipe(prior);
    this.rows.delete(groupIdB64);
    if (this.backend !== null) await this.backend.save(account, this.rows);
  }
}

/** The app's store; the journal-ownership provider binds it to the account. */
export const pendingRootSeeds = new PendingRootSeedStore();
