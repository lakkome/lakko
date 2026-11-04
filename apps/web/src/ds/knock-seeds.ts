// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 5 — a group's knock key.
 *
 * A request to join is a sealed KeyPackage, sealed to the group's knock key:
 * `knock_seed(v) = BLAKE3-derive-key("lakko/group/knock/v1", journal_root_seed
 * ‖ v)`. The owner derives every version from the ADR-0057 journal root seed
 * (so every device of the owner holds it with nothing to transfer); admins get
 * `knock_seed(v)` from the owner in a knock-seed envelope, sealed to their
 * account key, and keep it here. A link's capability and a public group's
 * record carry `(knock_pk, v)`; an admin opens a row with the seed of the
 * row's own version (`knockRowVersion`), so a version this device has never
 * heard of is a row it cannot read, never a guess.
 *
 * ## Which version is current
 *
 * Per group, persisted beside the seeds ({@link KnockSeedStore}). The owner's
 * device records the admin set its current version was handed to; when an
 * admin of that set is no longer an admin (removed or demoted), the version
 * goes up by one and only the remaining admins (and the owner's own other
 * devices) get the new seed. New `/g/` links and the public record carry the
 * current version and its key. Older versions stay derivable by the owner and
 * held by the admins that had them, so a request sealed to an older key still
 * opens ({@link knockSeedsForRow}).
 *
 * The owner's other devices learn a bump from the clear header of any
 * knock-seed envelope the owner sent: the version, and (from its public key)
 * which of the account's journal roots it was made under
 * ({@link KnockSeedStore.learnOwnerRoot}). They derive every version from
 * their own copies of the account's identities, predecessors included.
 *
 * Pure apart from the storage port; the envelope receive gate is
 * {@link knockSeedVerdict}.
 */

import { createSignal } from "solid-js";

import { registerAccountMemory } from "../auth/account-memory";
import { loadAccountJson, saveAccountJson } from "../storage/account-kv";

/** The first knock version of a group, and the label a request found by
 *  search carries (the public record names no version). */
export const INITIAL_KNOCK_VERSION = 0;

/** The highest version a row may name (two bytes on the wire). */
const MAX_KNOCK_VERSION = 0xffff;

const ROW_NAME = "knock-seeds";

/** What the knock-seed envelope's clear header says. */
export interface KnockSeedHeader {
  readonly journalId: Uint8Array;
  readonly memberAccountKey: Uint8Array;
  readonly knockVersion: number;
  readonly knockPublicKey: Uint8Array;
}

export type KnockSeedVerdict = "accept" | "malformed" | "wrong-journal" | "not-addressed" | "sender-not-owner";

function sameBytes(a: Uint8Array | undefined, b: Uint8Array | undefined): boolean {
  if (a === undefined || b === undefined || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** The receive gate: a seed is kept only when the envelope names this group
 *  and this account, and the sender resolves (at receipt, C-68) to the live
 *  role table's owner. */
export function knockSeedVerdict(args: {
  readonly header: KnockSeedHeader | null;
  readonly groupId: Uint8Array;
  readonly ownAccountKey: Uint8Array;
  readonly senderAccountKey: Uint8Array | undefined;
  readonly ownerAccountKey: Uint8Array | undefined;
}): KnockSeedVerdict {
  const { header } = args;
  if (header === null) return "malformed";
  if (!sameBytes(header.journalId, args.groupId)) return "wrong-journal";
  if (!sameBytes(header.memberAccountKey, args.ownAccountKey)) return "not-addressed";
  if (!sameBytes(args.senderAccountKey, args.ownerAccountKey)) return "sender-not-owner";
  return "accept";
}

function b64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i] ?? 0);
  return btoa(bin);
}

function unb64(s: string): Uint8Array | null {
  try {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/** group id (b64) -> version -> seed. */
type Rows = Map<string, Map<number, Uint8Array>>;

/** From which version on a knock seed is derived from which account
 *  identity's journal root (its signature key, base64). ADR-0087 section 5:
 *  an ADR-0074 rotation moves the journal root, so the versions before the
 *  rotation stay with the predecessor's root. */
export interface KnockRootBoundary {
  readonly from: number;
  readonly key: string;
}

/** The owner's state of one group: the current version, the admins it was
 *  handed to (`null` until the next reconcile records them), and the root
 *  boundaries (empty: every version from the current account identity). */
export interface OwnerKnockState {
  readonly version: number;
  readonly admins: readonly string[] | null;
  readonly roots?: readonly KnockRootBoundary[];
}

/** The root key of version `v`, or `null` for the current account identity.
 *  Pure. */
export function rootKeyForVersion(roots: readonly KnockRootBoundary[] | undefined, v: number): string | null {
  let key: string | null = null;
  for (const b of roots ?? []) if (b.from <= v) key = b.key;
  return key;
}

/** The state after a key rotation from `oldKey` to `newKey`: one version up,
 *  that version and later from the new root. `null` when it is already done
 *  (the last boundary names the new key). Pure. */
export function rotatedKnockState(
  held: OwnerKnockState | undefined,
  oldKey: string,
  newKey: string,
): OwnerKnockState | null {
  const roots = held?.roots ?? [];
  const last = roots[roots.length - 1];
  if (last !== undefined && last.key === newKey) return null;
  const current = held?.version ?? INITIAL_KNOCK_VERSION;
  if (current >= MAX_KNOCK_VERSION) return null;
  const next = current + 1;
  const before = roots.length === 0 ? [{ from: INITIAL_KNOCK_VERSION, key: oldKey }] : [...roots];
  return { version: next, admins: held?.admins ?? null, roots: [...before, { from: next, key: newKey }] };
}

type OwnerRows = Map<string, OwnerKnockState>;

function encodeRows(rows: Rows, owner: OwnerRows): unknown {
  const out: Record<string, Record<string, string>> = {};
  for (const [g, byVersion] of rows) {
    const inner: Record<string, string> = {};
    for (const [v, seed] of byVersion) inner[String(v)] = b64(seed);
    out[g] = inner;
  }
  const o: Record<string, { v: number; a: string[] | null; r?: { f: number; k: string }[] }> = {};
  for (const [g, st] of owner) {
    o[g] = {
      v: st.version,
      a: st.admins === null ? null : [...st.admins],
      ...(st.roots !== undefined && st.roots.length > 0 ? { r: st.roots.map((b) => ({ f: b.from, k: b.key })) } : {}),
    };
  }
  return { v: 2, g: out, o };
}

function decodeOwner(raw: unknown): OwnerRows {
  const rows: OwnerRows = new Map();
  if (raw === null || typeof raw !== "object") return rows;
  const o = (raw as { o?: unknown }).o;
  if (o === null || typeof o !== "object") return rows;
  for (const [g, st] of Object.entries(o as Record<string, unknown>)) {
    if (st === null || typeof st !== "object") continue;
    const v = (st as { v?: unknown }).v;
    const a = (st as { a?: unknown }).a;
    if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v > MAX_KNOCK_VERSION) continue;
    const admins = Array.isArray(a) ? a.filter((x): x is string => typeof x === "string") : null;
    const r = (st as { r?: unknown }).r;
    const roots: KnockRootBoundary[] = [];
    if (Array.isArray(r)) {
      for (const b of r) {
        const f = (b as { f?: unknown })?.f;
        const k = (b as { k?: unknown })?.k;
        if (typeof f === "number" && Number.isInteger(f) && f >= 0 && f <= MAX_KNOCK_VERSION && typeof k === "string") {
          roots.push({ from: f, key: k });
        }
      }
    }
    rows.set(g, { version: v, admins, ...(roots.length > 0 ? { roots } : {}) });
  }
  return rows;
}

function decodeRows(raw: unknown): Rows {
  const rows: Rows = new Map();
  if (raw === null || typeof raw !== "object") return rows;
  const obj = raw as { v?: unknown; g?: unknown };
  if ((obj.v !== 1 && obj.v !== 2) || obj.g === null || typeof obj.g !== "object") return rows;
  for (const [g, inner] of Object.entries(obj.g as Record<string, unknown>)) {
    if (inner === null || typeof inner !== "object") continue;
    const byVersion = new Map<number, Uint8Array>();
    for (const [vs, seedB64] of Object.entries(inner as Record<string, unknown>)) {
      const v = Number(vs);
      if (!Number.isInteger(v) || v < 0 || v > 0xffff || typeof seedB64 !== "string") continue;
      const seed = unb64(seedB64);
      if (seed !== null && seed.length === 32) byVersion.set(v, seed);
    }
    if (byVersion.size > 0) rows.set(g, byVersion);
  }
  return rows;
}

/** What an owner device records on hearing its own account's knock-seed
 *  envelope for `version` made under `rootKeyB64`. `null`: nothing to
 *  record. Pure. */
export function learnedOwnerState(
  held: OwnerKnockState | undefined,
  version: number,
  rootKeyB64: string | null,
  currentKeyB64: string,
): OwnerKnockState | null {
  const current = held?.version ?? INITIAL_KNOCK_VERSION;
  const roots = held?.roots ?? [];
  let nextRoots = roots;
  const keyAt = (v: number): string => rootKeyForVersion(roots, v) ?? currentKeyB64;
  if (rootKeyB64 !== null && keyAt(version) !== rootKeyB64) {
    // Only this version changes root: the owner sends every version, in any
    // order, and one older envelope must not move the newer ones.
    const keyAfter = (v: number): string => (v === version ? rootKeyB64 : keyAt(v));
    // Up to the highest version known: a later version keeps the root of the
    // one before it (`rootKeyForVersion`), so above that nothing is written.
    const last = Math.max(version, current, ...roots.map((b) => b.from));
    const placed: KnockRootBoundary[] = [];
    let prev = currentKeyB64;
    for (let v = 0; v <= last; v++) {
      const k = keyAfter(v);
      if (k !== prev) placed.push({ from: v, key: k });
      prev = k;
    }
    nextRoots = placed;
  }
  if (version <= current && nextRoots === roots) return null;
  const raised = version > current;
  return {
    version: Math.max(current, version),
    // A raised version records the admin set afresh at the next reconcile.
    admins: raised ? null : (held?.admins ?? null),
    ...(nextRoots.length > 0 ? { roots: nextRoots } : {}),
  };
}

/** The decision of {@link KnockSeedStore.reconcileOwnerAdmins}, pure. */
export function ownerKnockStep(
  held: OwnerKnockState | undefined,
  admins: ReadonlySet<string>,
): { readonly next: OwnerKnockState; readonly bumped: boolean } {
  const current = held?.version ?? INITIAL_KNOCK_VERSION;
  const sorted = [...admins].sort();
  if (held === undefined || held.admins === null) {
    const roots = held?.roots === undefined ? {} : { roots: held.roots };
    return { next: { version: current, admins: sorted, ...roots }, bumped: false };
  }
  const roots = held.roots === undefined ? {} : { roots: held.roots };
  const left = held.admins.some((a) => !admins.has(a));
  if (left && current < MAX_KNOCK_VERSION) {
    return { next: { version: current + 1, admins: sorted, ...roots }, bumped: true };
  }
  // A new admin joins the current version: no bump.
  return { next: { version: current, admins: sorted, ...roots }, bumped: false };
}

/** The knock seeds this device received as an admin, and the owner's current
 *  version, per group. */
export class KnockSeedStore {
  private account: string | null = null;
  private rows: Rows = new Map();
  private owner: OwnerRows = new Map();
  private opened: Promise<void> = Promise.resolve();
  /** Writes in order, so a bump never loses to an older write. */
  private chain: Promise<void> = Promise.resolve();

  open(accountId: string): Promise<void> {
    if (this.account === accountId) return this.opened;
    this.account = accountId;
    this.rows = new Map();
    this.owner = new Map();
    this.opened = loadAccountJson(accountId, ROW_NAME).then(
      (raw) => {
        if (this.account === accountId) {
          this.rows = decodeRows(raw);
          this.owner = decodeOwner(raw);
        }
      },
      () => undefined,
    );
    return this.opened;
  }

  close(): void {
    this.account = null;
    this.rows = new Map();
    this.owner = new Map();
  }

  private save(accountId: string): Promise<void> {
    const body = encodeRows(this.rows, this.owner);
    const next = this.chain.then(() => saveAccountJson(accountId, ROW_NAME, body));
    this.chain = next.catch(() => undefined);
    return next;
  }

  /** The version new links and the public record carry, for a group this
   *  account owns. */
  async ownerVersion(groupIdB64: string): Promise<number> {
    await this.opened;
    return this.owner.get(groupIdB64)?.version ?? INITIAL_KNOCK_VERSION;
  }

  /** The owner's reconcile: record the admin set, bumping the version when
   *  an admin it was handed to is no longer one. */
  async reconcileOwnerAdmins(
    accountId: string,
    groupIdB64: string,
    admins: ReadonlySet<string>,
    /** Runs, and must land, before a bump is written (the owed republish of
     *  the public record). A throw leaves the version where it was. */
    beforeBump?: () => Promise<void>,
  ): Promise<{ readonly version: number; readonly bumped: boolean }> {
    await this.open(accountId);
    const held = this.owner.get(groupIdB64);
    const step = ownerKnockStep(held, admins);
    if (step.bumped && beforeBump !== undefined) await beforeBump();
    const same =
      held !== undefined &&
      held.version === step.next.version &&
      held.admins !== null &&
      held.admins.join("|") === (step.next.admins ?? []).join("|");
    if (!same) {
      this.owner = new Map(this.owner).set(groupIdB64, step.next);
      await this.save(accountId);
    }
    return { version: step.next.version, bumped: step.bumped };
  }

  /** The root key (base64 signature key) a version's seed is derived from,
   *  or `null` for the current account identity. */
  async ownerRootKey(groupIdB64: string, version: number): Promise<string | null> {
    await this.opened;
    return rootKeyForVersion(this.owner.get(groupIdB64)?.roots, version);
  }

  /** ADR-0087 section 5: an owner's key rotation moved the journal root. One
   *  version up from the new root; earlier versions stay with the old one.
   *  `beforeBump` (the owed republish) lands first. Idempotent. */
  async rotateOwnerRoot(
    accountId: string,
    groupIdB64: string,
    oldKeyB64: string,
    newKeyB64: string,
    beforeBump?: () => Promise<void>,
  ): Promise<{ readonly version: number; readonly bumped: boolean }> {
    await this.open(accountId);
    const held = this.owner.get(groupIdB64);
    const next = rotatedKnockState(held, oldKeyB64, newKeyB64);
    if (next === null) return { version: held?.version ?? INITIAL_KNOCK_VERSION, bumped: false };
    if (beforeBump !== undefined) await beforeBump();
    this.owner = new Map(this.owner).set(groupIdB64, next);
    await this.save(accountId);
    return { version: next.version, bumped: true };
  }

  /** Another device of the owner sent a knock-seed envelope: its clear
   *  header names the version, and its public key tells which own journal
   *  root (`rootKeyB64`, `null` when none matched) it was made under. Raises
   *  the version and records the boundary, so this device mints links with
   *  the current version from the right root (ADR-0087 section 5). */
  async learnOwnerRoot(
    accountId: string,
    groupIdB64: string,
    version: number,
    rootKeyB64: string | null,
    currentKeyB64: string,
  ): Promise<void> {
    await this.open(accountId);
    if (!Number.isInteger(version) || version < 0 || version > MAX_KNOCK_VERSION) return;
    const st = this.owner.get(groupIdB64);
    const held = st?.version ?? INITIAL_KNOCK_VERSION;
    const next = learnedOwnerState(st, version, rootKeyB64, currentKeyB64);
    if (next === null) return;
    if (next.version === held && JSON.stringify(next.roots ?? []) === JSON.stringify(st?.roots ?? [])) return;
    this.owner = new Map(this.owner).set(groupIdB64, next);
    await this.save(accountId);
  }

  /** The versions this device holds a seed for (as an admin), newest first. */
  async heldVersions(groupIdB64: string): Promise<number[]> {
    await this.opened;
    return [...(this.rows.get(groupIdB64)?.keys() ?? [])].sort((a, b) => b - a);
  }

  async seedFor(groupIdB64: string, version: number): Promise<Uint8Array | null> {
    await this.opened;
    return this.rows.get(groupIdB64)?.get(version) ?? null;
  }

  async latest(groupIdB64: string): Promise<{ version: number; seed: Uint8Array } | null> {
    await this.opened;
    const byVersion = this.rows.get(groupIdB64);
    if (byVersion === undefined) return null;
    let best: { version: number; seed: Uint8Array } | null = null;
    for (const [version, seed] of byVersion) if (best === null || version > best.version) best = { version, seed };
    return best;
  }

  async keep(accountId: string, groupIdB64: string, version: number, seed: Uint8Array): Promise<void> {
    await this.open(accountId);
    const byVersion = new Map(this.rows.get(groupIdB64) ?? []);
    // The first seed given for a version is kept, never replaced.
    if (byVersion.has(version)) return;
    byVersion.set(version, seed);
    this.rows = new Map(this.rows).set(groupIdB64, byVersion);
    await this.save(accountId);
  }

  async forget(accountId: string, groupIdB64: string): Promise<void> {
    await this.open(accountId);
    if (!this.rows.has(groupIdB64) && !this.owner.has(groupIdB64)) return;
    const next = new Map(this.rows);
    next.delete(groupIdB64);
    this.rows = next;
    const owner = new Map(this.owner);
    owner.delete(groupIdB64);
    this.owner = owner;
    await this.save(accountId);
  }
}

export const knockSeeds = new KnockSeedStore();

registerAccountMemory("knock-seeds", () => knockSeeds.close());

/** Which (group, admin, version) this owner device has sent a seed to this
 *  session, so the reconcile sends each once. */
const [sentVersion, setSentVersion] = createSignal(0);
const sent = new Set<string>();

export function knockSeedSent(groupIdB64: string, adminKeyB64: string, version: number): boolean {
  sentVersion();
  return sent.has(`${groupIdB64}|${adminKeyB64}|${version}`);
}

export function noteKnockSeedSent(groupIdB64: string, adminKeyB64: string, version: number): void {
  sent.add(`${groupIdB64}|${adminKeyB64}|${version}`);
  setSentVersion((v) => v + 1);
}

registerAccountMemory("knock-seed-sent", () => sent.clear());

/** One knock-seed envelope as the receive path hands it over (at receipt). */
export interface KnockSeedArrival {
  readonly groupId: Uint8Array;
  readonly groupIdB64: string;
  readonly plaintext: Uint8Array;
  /** The MLS sender's account key, resolved at receipt (C-68). */
  readonly senderAccountKey: Uint8Array | undefined;
  /** The live role table's owner, read at receipt. */
  readonly ownerAccountKey: Uint8Array | undefined;
}

type KnockSeedHandler = (arrival: KnockSeedArrival) => Promise<KnockSeedVerdict>;

let knockHandler: KnockSeedHandler | null = null;

/** Install the handler (the chat context does, once an account is signed
 *  in); returns the uninstaller. */
export function installKnockSeedHandler(h: KnockSeedHandler): () => void {
  knockHandler = h;
  return () => {
    if (knockHandler === h) knockHandler = null;
  };
}

/** Hand one envelope over. Without a handler it is dropped. Never throws. */
export async function deliverKnockSeed(arrival: KnockSeedArrival): Promise<KnockSeedVerdict> {
  if (knockHandler === null) return "not-addressed";
  try {
    return await knockHandler(arrival);
  } catch {
    return "malformed";
  }
}

/** The owner's knock seeds for a group and a version (derived from the
 *  journal root seed), the boundary's root first, then the same version under
 *  every other own identity (an owner device that never saw the boundary);
 *  empty when this account does not own the group. Installed by the
 *  journal-ownership context, which holds the account identities. */
type OwnerSeedSource = (groupId: Uint8Array, version: number, rootKeyB64: string | null) => Uint8Array[];
let ownerSeedSource: OwnerSeedSource | null = null;

export function installOwnerKnockSeedSource(source: OwnerSeedSource | null): void {
  ownerSeedSource = source;
}

/** The knock seeds that may open a row naming `version`. Every row names its
 *  own version: a link carries it in its capability and a public record in
 *  its payload. The owner derives that version from the journal root it was
 *  made under (the recorded boundary, then every other own identity, for an
 *  owner device that never saw the boundary); an admin uses the seed it was
 *  handed for that version. */
export async function knockSeedsForRow(
  groupId: Uint8Array,
  groupIdB64: string,
  version: number,
): Promise<Uint8Array[]> {
  try {
    const own = ownerSeedSource?.(groupId, version, await knockSeeds.ownerRootKey(groupIdB64, version)) ?? [];
    if (own.length > 0) return own;
  } catch {
    /* not the owner, or a version past the wire bound */
  }
  const held = await knockSeeds.seedFor(groupIdB64, version);
  return held === null ? [] : [held];
}

/** The owner's seed of `version`, from the root its boundary names; `null`
 *  when no source is installed or this account does not own the group. */
export async function ownerKnockSeed(
  groupId: Uint8Array,
  groupIdB64: string,
  version: number,
): Promise<Uint8Array | null> {
  try {
    const seeds = ownerSeedSource?.(groupId, version, await knockSeeds.ownerRootKey(groupIdB64, version)) ?? [];
    for (const other of seeds.slice(1)) other.fill(0);
    return seeds[0] ?? null;
  } catch {
    return null;
  }
}

/** Whether a group is public (its `@name` flag, as the group-name context
 *  holds it). A private group takes a knock row only with a capability of
 *  this group (ADR-0087 section 5). Installed by the group-name context. */
let publicLookup: ((groupIdB64: string) => boolean) | null = null;

export function installGroupPublicLookup(fn: ((groupIdB64: string) => boolean) | null): void {
  publicLookup = fn;
}

export function groupIsPublic(groupIdB64: string): boolean {
  return publicLookup?.(groupIdB64) === true;
}

/** UX section 8 — the name a request carries, by the joiner's leaf key
 *  (base64): what the person claims, unverified, shown with no mark. Memory
 *  only; the retained request itself is durable in the MLS snapshot. */
const [claimedVersion, setClaimedVersion] = createSignal(0);
const claimedNames = new Map<string, string>();

export function rememberKnockName(joinerKeyB64: string, name: string): void {
  if (name.trim().length === 0) return;
  claimedNames.set(joinerKeyB64, name.trim());
  setClaimedVersion((v) => v + 1);
}

export function knockClaimedName(joinerKeyB64: string): string | null {
  claimedVersion();
  return claimedNames.get(joinerKeyB64) ?? null;
}

registerAccountMemory("knock-claimed-names", () => claimedNames.clear());
