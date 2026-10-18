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
 * a rank shortfall, 409 a conflict or a lock race (retried once). The push
 * binding of ADR-0057 is gone (ADR-0078 section 3): a wake is sealed to
 * push-gw and signed by the group's wake key, and the DS binds it to nothing.
 *
 * ## Append and Replace (the second security pass, 2026-09-28)
 *
 * Every commit path registers its next epoch in APPEND mode, and since
 * ADR-0081 it does so in the same request that appends the commit
 * (`commit`, `POST /ds/group/<hex>/commit`, {@link commitEpochWith}): the DS
 * accepts the registration only at the journal's `latest + 1` and only
 * together with the row, so a registered epoch without its commit cannot
 * exist. Re-sending the same row is idempotent (it answers the original
 * id). A 409 means another commit owns that epoch: the commit is abandoned
 * exactly like a lost publish race and is never retried as a Replace. No
 * client sends a plain `registerEpoch` any more (ADR-0081 open question 1
 * asks whether the DS should refuse one). REPLACE
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
  /** ADR-0081 - `POST .../commit`: the append-mode registration of
   *  `epochKey` at `epoch` and the commit `row`, one request. */
  commit(
    journalId: Uint8Array,
    epoch: bigint,
    epochKey: Uint8Array,
    revokeKeys: Uint8Array,
    row: Uint8Array,
  ): StatementRequestLike;
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

// ----------------------------------------------------------------------------
// ADR-0081: register an epoch and append its commit in one request
// ----------------------------------------------------------------------------

/** How the DS answered a commit: the journal row id the commit has, and
 *  whether this request was an identical re-send of one it already took
 *  (`200`) rather than a new append (`201`). */
export interface CommitAnswer {
  readonly id: number;
  readonly createdAt: string;
  readonly resent: boolean;
}

/** The request proof signer of the epoch the commit leaves (structural: the
 *  wasm `DsProofSigner`). */
export interface CommitProofSigner {
  readonly epoch: bigint;
  sign(method: string, path: string, query: string, body: Uint8Array): string;
}

/** The one network call a commit needs: `POST <path>` with `bodyJson`,
 *  proven by `proof`. Throws the DS client's HTTP error (numeric `status`). */
export interface CommitSender {
  postCommit(path: string, bodyJson: string, proof: CommitProofSigner): Promise<CommitAnswer>;
}

/** The DS refused the commit with 409 twice: another commit owns the epoch.
 *  Nothing of this commit is on the DS. */
export class CommitConflictError extends Error {
  override name = "CommitConflictError";
  readonly status = 409;
  constructor(public readonly cause: unknown) {
    super("ds: another commit already moved the journal to this epoch");
  }
}

/** `true` when `err` is (or wraps) a lost commit: {@link CommitConflictError}
 *  or a bare 409 from the commit route. */
export function isCommitConflict(err: unknown): boolean {
  return err instanceof CommitConflictError || isStatus(err, 409);
}

/**
 * ADR-0081 decision 1 - register `epochKey` at `epoch` (APPEND, signed with
 * this device's authority, revoking `revokeKeys`) and append `row` in ONE
 * request, proven under `proof` (the key of epoch `epoch - 1`). A 409 is
 * retried ONCE with a freshly signed statement (it may be the server's lock
 * race; re-sending this same row is idempotent), then thrown as
 * {@link CommitConflictError}. Every attempt signs a new statement: a
 * statement's nonce is spent and its window is two minutes.
 */
export async function commitEpochWith(
  sender: CommitSender,
  journalId: Uint8Array,
  authority: JournalAuthority,
  epoch: bigint,
  epochKey: Uint8Array,
  revokeKeys: readonly Uint8Array[],
  row: Uint8Array,
  proof: CommitProofSigner,
): Promise<CommitAnswer> {
  if (authority.kind === "none") throw new NoJournalAuthorityError();
  if (row.length === 0) throw new Error("ds: a commit row must be non-empty");
  const flat = concatKeys(revokeKeys);
  const once = async (): Promise<CommitAnswer> => {
    const [path, body] = readRequest(authority.signer.commit(journalId, epoch, epochKey, flat, row));
    return sender.postCommit(path, body, proof);
  };
  try {
    return await once();
  } catch (err) {
    if (!isStatus(err, 409)) throw err;
  }
  try {
    return await once();
  } catch (err) {
    if (isStatus(err, 409)) throw new CommitConflictError(err);
    throw err;
  }
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

/**
 * A plain root transfer old -> new for a journal the account owns (its
 * DeviceGroup, an owned group, a PublisherGroup) after a key rotation.
 * `"already"` when the old root is refused and the new root's probe answers
 * 204 (a retry). Since ADR-0078 phase 6 the DeviceGroup takes this route too:
 * the attested transfer and its root endorsement are gone with the journal id
 * the key-reset attestation used to name.
 */
export async function transferRoot(
  sender: StatementSender,
  journalId: Uint8Array,
  oldRoot: Pick<StatementSignerLike, "rootTransfer">,
  newRoot: Pick<StatementSignerLike, "rootTransfer" | "publicKey">,
): Promise<"transferred" | "already"> {
  try {
    await sendStatement(sender, () => oldRoot.rootTransfer(journalId, newRoot.publicKey()));
    return "transferred";
  } catch (err) {
    if (!isStatus(err, 401)) throw err;
    const probe = await probeRoot(sender, journalId, newRoot);
    if (probe === "root") return "already";
    throw err;
  }
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
 * The registrar public keys a removal's registration revokes (ADR-0057,
 * amended 2026-10-01, item 1: whoever removes a person revokes their
 * registrar key in the same registration):
 *
 *   - the ROOT derives one per evicted account from the root seed;
 *   - a REGISTRAR (an admin committing a removal) takes the keys it collected
 *     from the owner's registrar-seed envelopes (`registrar-list.ts`),
 *     keeping only those of a STRICTLY lower rank than its own: the DS
 *     refuses a statement that revokes an equal or higher rank, and one
 *     refused statement would abort the whole commit. Removing an admin's
 *     key stays the owner's, at its next `registrars-set`;
 *   - with no authority, nothing.
 */
export function revocationKeys(
  authority: JournalAuthority,
  evictees: readonly Uint8Array[],
  derive: RegistrarDerivation,
  registrarList: ReadonlyMap<string, { readonly publicKey: Uint8Array; readonly rank: number }> = new Map(),
): Uint8Array[] {
  if (authority.kind === "root") {
    return evictees.map((account) =>
      derive.publicKeyOfSeed(derive.registrarSeed(authority.rootSeed, account)),
    );
  }
  if (authority.kind === "none") return [];
  const out: Uint8Array[] = [];
  for (const account of evictees) {
    const entry = registrarList.get(b64(account));
    if (entry === undefined) continue;
    // Equal or higher rank than the signer: the DS would refuse it.
    if (entry.rank >= authority.rank) continue;
    out.push(entry.publicKey);
  }
  return out;
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



