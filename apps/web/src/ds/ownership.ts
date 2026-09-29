// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 - a journal has an owner; only those who change a group register
 * its epochs. The client half of that contract that is not a network call.
 *
 * ## Who signs what
 *
 * A group journal on the Delivery Service is claimed under a ROOT key derived
 * on every device of the owner's account (`dsJournalRootSigner(accountIdentity,
 * journalId)`), and its epoch keys are registered only by the root or by the
 * REGISTRARS the root appoints (`dsStatementSigner(registrarSeed)`), in three
 * ranks: root > admin > member. An epoch key opens the journal and registers
 * nothing (ADR-0053's heal-on-apply is gone: a plain member that finds no key
 * for the epoch it reached waits for the committer's registration).
 *
 *   - **DeviceGroup**: the account owns it, so every device is root.
 *   - **PublisherGroup**: the channel owner is root. A co-publisher has no
 *     registrar seed on web (a PublisherGroup carries no application
 *     messages to deliver one in), so its commits are not registered.
 *   - **Conversation group**: the ADR-0022 owner is root; admins hold an
 *     admin registrar seed, every other member a member seed, both delivered
 *     by the owner in a sealed envelope (`registrar-seeds.ts`).
 *
 * ## The statements
 *
 * Every statement is `PUT /ds/group/<hex>/<segment>` with body
 * `{"statement_b64": ...}` and NO proof header; the wasm signer returns
 * `{path, bodyJson}`. Codes: 204 an identical re-send, 400 malformed, 401 a
 * bad or stranger signer (or an unclaimed journal on a non-claim route), 403
 * a rank shortfall, 409 a conflict or a lock race (retried once), 503
 * `push-binding-disabled`.
 *
 * ## Append and Replace (the second security pass, 2026-09-28)
 *
 * Every commit path registers its next epoch in APPEND mode
 * (`registerEpoch`): accepted only at the journal's `latest + 1` (an
 * identical re-send is 204), and a member-rank signer only one epoch past
 * the newest admin or root registration. A 409 there means someone
 * registered that epoch first: the commit is abandoned exactly like a lost
 * publish race and is never retried as a Replace. REPLACE
 * (`replaceEpoch`) is only the owner's or an admin's repair of a journal a
 * lower rank pushed ahead (`journal-repair.ts`); the server permits it when
 * everything above the epoch was registered by a strictly lower rank, with
 * no root override.
 *
 * Everything here takes its signers and its network through structural ports
 * so the host tests drive the rules with fakes.
 */

/** Registrar ranks on the wire (`registrars` / `resetWithRegistrars`). */
export const RANK_REMOVE = 0;
export const RANK_MEMBER = 1;
export const RANK_ADMIN = 2;

/** One statement the wasm signer built: where to `PUT` it and the exact body. */
export interface StatementRequestLike {
  readonly path: string;
  readonly bodyJson: string;
  free(): void;
}

/** The part of the wasm `DsStatementSigner` this client uses. */
export interface StatementSignerLike {
  claim(journalId: Uint8Array, epoch: bigint, epochKey: Uint8Array): StatementRequestLike;
  reset(journalId: Uint8Array, epoch: bigint, epochKey: Uint8Array): StatementRequestLike;
  registerEpoch(
    journalId: Uint8Array,
    epoch: bigint,
    epochKey: Uint8Array,
    revokeKeys: Uint8Array,
  ): StatementRequestLike;
  replaceEpoch(
    journalId: Uint8Array,
    epoch: bigint,
    epochKey: Uint8Array,
    revokeKeys: Uint8Array,
  ): StatementRequestLike;
  registrars(journalId: Uint8Array, keys: Uint8Array, ranks: Uint8Array): StatementRequestLike;
  /** `PUT .../registrars-set`: the root replaces the WHOLE registrar set. */
  registrarsSet(journalId: Uint8Array, keys: Uint8Array, ranks: Uint8Array): StatementRequestLike;
  /** `PUT .../root`. Naming the signer's own key is the "am I still the
   *  root?" probe: 204 while it is, the uniform 401 once it is not. */
  rootTransfer(journalId: Uint8Array, newRoot: Uint8Array): StatementRequestLike;
  pushBinding(journalId: Uint8Array, pushGid: Uint8Array): StatementRequestLike;
  publicKey(): Uint8Array;
  free(): void;
}

/**
 * What this device may sign for one journal. The signer is owned by whoever
 * resolved the authority and freed with {@link releaseAuthority}.
 */
export type JournalAuthority =
  | {
      readonly kind: "root";
      readonly signer: StatementSignerLike;
      /** The root seed, from which the owner derives any member's registrar
       *  seed (appointment, revocation). */
      readonly rootSeed: Uint8Array;
    }
  | {
      readonly kind: "registrar";
      readonly signer: StatementSignerLike;
      /** `RANK_MEMBER` or `RANK_ADMIN`, as the owner's envelope named it. */
      readonly rank: number;
    }
  | { readonly kind: "none" };

/** Free the signer an authority holds. Idempotent enough: a double free is
 *  swallowed. */
export function releaseAuthority(authority: JournalAuthority): void {
  if (authority.kind === "none") return;
  try {
    authority.signer.free();
  } catch {
    /* already freed */
  }
}

/** The one network call statements need: `PUT <path>` with `bodyJson`, no
 *  proof header. Throws the DS client's HTTP error (with a numeric `status`)
 *  on a refusal. */
export interface StatementSender {
  putStatement(path: string, bodyJson: string): Promise<void>;
}

/** This device holds neither the root nor a registrar seed for the journal,
 *  so it cannot register an epoch and must not commit (obligation 5). */
export class NoJournalAuthorityError extends Error {
  override name = "NoJournalAuthorityError";
  constructor() {
    super("ds ownership: this device may not register epochs for this group");
  }
}

function statusOf(err: unknown): number | null {
  if (err !== null && typeof err === "object") {
    const status = (err as { status?: unknown }).status;
    if (typeof status === "number") return status;
  }
  return null;
}

/** `true` when `err` is a DS refusal with `status`. */
export function isStatus(err: unknown, status: number): boolean {
  return statusOf(err) === status;
}

/**
 * Build one statement and send it; a 409 (a conflict, or the server's lock
 * race) is retried ONCE with a freshly built statement (a fresh nonce). The
 * request object is freed as soon as its path and body are read.
 */
export async function sendStatement(
  sender: StatementSender,
  build: () => StatementRequestLike,
): Promise<void> {
  const once = async (): Promise<void> => {
    const req = build();
    let path: string;
    let body: string;
    try {
      path = req.path;
      body = req.bodyJson;
    } finally {
      try {
        req.free();
      } catch {
        /* already freed */
      }
    }
    await sender.putStatement(path, body);
  };
  try {
    await once();
  } catch (err) {
    if (!isStatus(err, 409)) throw err;
    await once();
  }
}

/** Concatenate 32-byte keys into the flat form the statements take. */
export function concatKeys(keys: readonly Uint8Array[]): Uint8Array {
  const out = new Uint8Array(keys.length * 32);
  keys.forEach((k, i) => {
    if (k.length !== 32) throw new Error("ds ownership: a key is not 32 bytes");
    out.set(k, i * 32);
  });
  return out;
}

/**
 * Obligation 5 - register `epochKey` at `epoch` in APPEND mode with this
 * device's authority, revoking `revokeKeys` (registrar public keys) in the
 * same transaction. Throws {@link NoJournalAuthorityError} without an
 * authority, and the DS refusal on anything else (409: another committer
 * registered that epoch first); the caller aborts before publishing and never
 * retries as a Replace.
 */
export async function registerEpochWith(
  sender: StatementSender,
  journalId: Uint8Array,
  authority: JournalAuthority,
  epoch: bigint,
  epochKey: Uint8Array,
  revokeKeys: readonly Uint8Array[] = [],
): Promise<void> {
  if (authority.kind === "none") throw new NoJournalAuthorityError();
  const flat = concatKeys(revokeKeys);
  await sendStatement(sender, () =>
    authority.signer.registerEpoch(journalId, epoch, epochKey, flat),
  );
}

/**
 * The repair (`journal-repair.ts`): REPLACE at `epoch` with `epochKey`,
 * dropping every epoch above it. Only the root or an admin registrar sends
 * it; the server answers 403 when an equal or higher rank registered above.
 */
export async function replaceEpochWith(
  sender: StatementSender,
  journalId: Uint8Array,
  authority: JournalAuthority,
  epoch: bigint,
  epochKey: Uint8Array,
): Promise<void> {
  if (authority.kind === "none") throw new NoJournalAuthorityError();
  await sendStatement(sender, () =>
    authority.signer.replaceEpoch(journalId, epoch, epochKey, new Uint8Array(0)),
  );
}

/**
 * The "am I still the root?" probe: a `root` statement signed by `root`
 * naming `root`'s own key. `"root"` on 204, `"not-root"` on the uniform 401;
 * any other refusal or a network failure throws (the caller retries later).
 */
export async function probeRoot(
  sender: StatementSender,
  journalId: Uint8Array,
  root: Pick<StatementSignerLike, "rootTransfer" | "publicKey">,
): Promise<"root" | "not-root"> {
  try {
    await sendStatement(sender, () => root.rootTransfer(journalId, root.publicKey()));
    return "root";
  } catch (err) {
    if (isStatus(err, 401)) return "not-root";
    throw err;
  }
}

/**
 * Obligations 1 and 2 - the owner claims its journal with the root: at group
 * creation, and lazily when a signed request found the journal unclaimed. The
 * claim names the current epoch and its key, so it registers that epoch too.
 *
 * A 409 means the journal is ALREADY claimed under this root with another key
 * at that epoch (another device of the owner claimed it, or the chain moved
 * on): nothing more is sent, and the lazy claim leaves it there. Returns
 * which of the two it was. Creation does NOT accept "already-claimed" as a
 * success (it fails closed, see {@link claimAtCreation}).
 */
export async function claimAsOwner(
  sender: StatementSender,
  journalId: Uint8Array,
  root: StatementSignerLike,
  epoch: bigint,
  epochKey: Uint8Array,
): Promise<"claimed" | "already-claimed"> {
  try {
    await sender.putStatement(...readRequest(root.claim(journalId, epoch, epochKey)));
    return "claimed";
  } catch (err) {
    if (isStatus(err, 409)) return "already-claimed";
    throw err;
  }
}

/** The claim at creation did not land; nothing may be published. */
export class JournalClaimFailedError extends Error {
  override name = "JournalClaimFailedError";
  constructor(public readonly cause: unknown) {
    super("ds ownership: the new group's journal could not be claimed");
  }
}

/**
 * Finding 3 of the second security pass - a conversation group's creation
 * claims its (random-id) journal and gets 204 BEFORE any Welcome leaves; a
 * 409 (somebody else holds the id) or any failure throws
 * {@link JournalClaimFailedError} so the caller aborts the creation.
 */
export async function claimAtCreation(
  sender: StatementSender,
  journalId: Uint8Array,
  root: StatementSignerLike,
  epoch: bigint,
  epochKey: Uint8Array,
): Promise<void> {
  try {
    await sender.putStatement(...readRequest(root.claim(journalId, epoch, epochKey)));
  } catch (err) {
    throw new JournalClaimFailedError(err);
  }
}

/**
 * Obligation 7 - a DeviceGroup or PublisherGroup whose id is deterministic is
 * re-founded under the same id after a lost-state recovery. The first claim
 * of a fresh id is a claim; a claim the server refuses with 409 (the old
 * group's chain is still there, under the same root) becomes a root RESET,
 * which replaces the chain and keeps the registrar set.
 */
export async function claimOrReset(
  sender: StatementSender,
  journalId: Uint8Array,
  root: StatementSignerLike,
  epoch: bigint,
  epochKey: Uint8Array,
): Promise<"claimed" | "reset"> {
  try {
    await sender.putStatement(...readRequest(root.claim(journalId, epoch, epochKey)));
    return "claimed";
  } catch (err) {
    if (!isStatus(err, 409)) throw err;
  }
  await sendStatement(sender, () => root.reset(journalId, epoch, epochKey));
  return "reset";
}

function readRequest(req: StatementRequestLike): [string, string] {
  try {
    return [req.path, req.bodyJson];
  } finally {
    try {
      req.free();
    } catch {
      /* already freed */
    }
  }
}

// ----------------------------------------------------------------------------
// Removal: who leaves, and which registrar keys go with them
// ----------------------------------------------------------------------------

/** The roster fields the removal rule reads (`Group.members()`). */
export interface OwnershipRosterMember {
  readonly leafIndex: number;
  readonly signaturePublicKey: Uint8Array;
  /** ADR-0050 - the account behind the leaf. */
  readonly accountKey?: Uint8Array;
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] ?? 0);
  return btoa(s);
}

function accountOf(m: OwnershipRosterMember): Uint8Array {
  return m.accountKey !== undefined && m.accountKey.length === 32
    ? m.accountKey
    : m.signaturePublicKey;
}

/**
 * The accounts a commit removing `removedLeaves` evicts: an account leaves the
 * group only when EVERY one of its leaves is removed (ADR-0050: removing one
 * device of a person keeps the person, and their registrar key).
 */
export function evicteeAccountKeys(
  roster: readonly OwnershipRosterMember[],
  removedLeaves: ReadonlySet<number>,
): Uint8Array[] {
  const byAccount = new Map<string, { key: Uint8Array; remaining: number }>();
  for (const m of roster) {
    const key = accountOf(m);
    const id = b64(key);
    const row = byAccount.get(id) ?? { key, remaining: 0 };
    if (!removedLeaves.has(m.leafIndex)) row.remaining += 1;
    byAccount.set(id, row);
  }
  const out: Uint8Array[] = [];
  for (const [, row] of byAccount) {
    if (row.remaining === 0) out.push(row.key);
  }
  return out;
}

/** The wasm derivations the owner needs, as a port. */
export interface RegistrarDerivation {
  /** `dsJournalRegistrarSeed(rootSeed, memberAccountKey)`. */
  registrarSeed(rootSeed: Uint8Array, memberAccountKey: Uint8Array): Uint8Array;
  /** `dsStatementSigner(seed).publicKey()`, the signer freed. */
  publicKeyOfSeed(seed: Uint8Array): Uint8Array;
}

/**
 * The registrar public keys the owner revokes with a removal's registration:
 * one per evicted account, derived from the root seed. An admin (no root seed)
 * derives nothing and sends an empty list; the owner revokes on its next
 * reconcile (the ADR residual).
 */
export function revocationKeys(
  authority: JournalAuthority,
  evictees: readonly Uint8Array[],
  derive: RegistrarDerivation,
): Uint8Array[] {
  if (authority.kind !== "root") return [];
  return evictees.map((account) =>
    derive.publicKeyOfSeed(derive.registrarSeed(authority.rootSeed, account)),
  );
}

// ----------------------------------------------------------------------------
// Appointment (obligation 3): the owner's desired registrar set
// ----------------------------------------------------------------------------

/**
 * Every non-owner account in the roster, at `RANK_ADMIN` when the role table
 * names it an admin and `RANK_MEMBER` otherwise. Keys are base64 account
 * keys.
 */
export function desiredRegistrars(args: {
  readonly memberAccountKeysB64: Iterable<string>;
  readonly ownerAccountKeyB64: string;
  readonly adminAccountKeysB64: ReadonlySet<string>;
}): Map<string, number> {
  const out = new Map<string, number>();
  for (const key of args.memberAccountKeysB64) {
    if (key === args.ownerAccountKeyB64) continue;
    out.set(key, args.adminAccountKeysB64.has(key) ? RANK_ADMIN : RANK_MEMBER);
  }
  return out;
}

// ----------------------------------------------------------------------------
// Push binding (obligation 8)
// ----------------------------------------------------------------------------

/** Who binds a push routing id: the root or an admin registrar. A member
 *  registrar would be refused (403), so it does not try. */
export function mayBindPush(authority: JournalAuthority): boolean {
  return authority.kind === "root" || (authority.kind === "registrar" && authority.rank === RANK_ADMIN);
}

/** What a push binding attempt came to. 409 (bound to another journal) and
 *  503 (binding disabled on this server) are logged and the caller continues:
 *  wakes for this group are dropped until fixed, foreground polling still
 *  delivers. */
export type PushBindingOutcome = "bound" | "not-permitted" | "conflict" | "disabled" | "failed";

/**
 * Obligation 8 - when the owner or an admin mints or rotates the group's push
 * routing secret, bind the journal to its `push_gid` BEFORE the secret is
 * broadcast (finding 5 of the second security pass: binding after the
 * broadcast let any member who read the secret squat the id first). Never
 * throws.
 */
export async function bindPushRouting(
  sender: StatementSender,
  journalId: Uint8Array,
  authority: JournalAuthority,
  pushGid: Uint8Array,
): Promise<PushBindingOutcome> {
  if (authority.kind === "none" || !mayBindPush(authority)) return "not-permitted";
  try {
    await sender.putStatement(...readRequest(authority.signer.pushBinding(journalId, pushGid)));
    return "bound";
  } catch (err) {
    const status = statusOf(err);
    if (status === 409) {
      // eslint-disable-next-line no-console
      console.warn("ds: push binding refused (409), the routing id is bound to another journal");
      return "conflict";
    }
    if (status === 503) {
      // eslint-disable-next-line no-console
      console.warn("ds: push binding unavailable (503 push-binding-disabled)");
      return "disabled";
    }
    // eslint-disable-next-line no-console
    console.warn(`ds: push binding failed (${status === null ? "network" : `HTTP ${status}`})`);
    return "failed";
  }
}
