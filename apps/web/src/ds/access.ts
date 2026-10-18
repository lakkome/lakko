// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 - a journal on the Delivery Service is opened by a key, not by its
 * name. This module holds the client half of that contract that is not a
 * network call: which signer proves a request to which journal, the rules the
 * DS client follows when a proof is refused, and the pure decisions the call
 * sites share (register-before-publish, the clock-skew copy, the knock
 * sequence), and, since ADR-0057, which AUTHORITY this device holds over a
 * journal: its root (the owner), a registrar seed the owner appointed, or
 * none. An epoch key opens a journal and registers nothing; only the root and
 * registrars register epochs (`./ownership`).
 *
 * ## Where a signer comes from
 *
 * A group journal is proven under a per-epoch Ed25519 key derived from the
 * MLS group itself (`group.dsAccessSigner(identity)`); a mailbox under the
 * identity key its pseudonym is the hash of (`dsMailboxSigner(identity)`).
 * The DS client does not hold groups, so the owners of live groups register a
 * **source** here - a function that, given a `group_id`, derives the signer of
 * the epoch that group is at NOW (ChatContext for conversation groups, the
 * DeviceGroup sync provider for the DeviceGroup, the co-publisher panel for a
 * PublisherGroup). A caller that owns a short-lived group (a one-shot
 * DeviceGroup commit, the register-before-publish window) passes the signer
 * explicitly instead.
 *
 * ## Why the registry keeps the PREVIOUS signer
 *
 * Contract point 5: a request refused under the current epoch's key is retried
 * once under the previous epoch's key, because the device may have applied a
 * commit whose epoch key the server does not hold yet (the one race register-
 * before-publish leaves: a losing committer replaced the key). So every time a
 * source reports a signer for a different epoch, the old one moves to
 * `previous` rather than being dropped.
 *
 * ## Lifetime
 *
 * Signers are wasm objects. Every signer the registry holds is the registry's
 * to free, and it never hands one out: signing is synchronous, so the header
 * is computed at the moment of use and nothing can free the signer under it.
 */
import { t } from "../l10n";

import { OwnerClaimMemo } from "./owner-claim";
import { releaseAuthority, type CommitAnswer, type JournalAuthority } from "./ownership";

/** The header a proof travels in (`dsProofHeaderName()` on the wasm surface;
 *  pinned against it by `access.test.ts`). */
export const DS_PROOF_HEADER = "x-lakko-ds-proof";

/** Contract point 5: a refusal whose response `Date` differs from the device
 *  clock by more than this is reported as a clock problem. */
export const CLOCK_SKEW_LIMIT_MS = 60_000;

/** The copy a user sees when a refusal is explained by the device clock. */
export const CLOCK_SKEW_NOTICE = (): string => t("ds.clock_skew");

/**
 * The part of the wasm `DsProofSigner` this client uses. Structural, so the
 * host tests can drive the rules with a fake.
 */
export interface ProofSigner {
  readonly epoch: bigint;
  sign(method: string, path: string, query: string, body: Uint8Array): string;
  mailboxClaimBodyJson(): string;
  verifyingKey(): Uint8Array;
  /** The 32-byte address of the mailbox a mailbox signer's key names
   *  (ADR-0078: derived from the mailbox key, never the identity key). */
  mailboxAddress(): Uint8Array;
  free(): void;
}

/** Derive the signer of the epoch a group is at now, or `null` when this
 *  source does not hold the group (or cannot derive: an evicted group). The
 *  returned signer is handed over: the registry frees it. */
export type GroupSignerSource = (groupId: Uint8Array) => ProofSigner | null | undefined;

/**
 * ADR-0057 - does THIS account own the journal of `groupId` (hold its root)?
 * `true` / `false` for a group this source holds, `null` for one it does not
 * know. Chat groups answer from the live role table, the DeviceGroup source
 * answers `true` for its own id, the channel owner's PublisherGroup `true`.
 */
export type JournalOwnerSource = (groupId: Uint8Array) => boolean | null | undefined;

/** Builds the statement signers (wasm) for the registry, installed by the
 *  account's session (`ds/journal-authority.ts`). */
export interface JournalAuthorityFactory {
  /** The root authority of the signed-in account over `groupId`, or `none`. */
  root(groupId: Uint8Array): JournalAuthority;
  /** The registrar authority from a seed the owner handed this account, or
   *  `none`. */
  registrar(groupId: Uint8Array): JournalAuthority;
  /** ADR-0057 second security pass, finding 4 - is this account known to
   *  hold the journal root (`root-held.ts`)? Absent = always. */
  rootHeld?(groupId: Uint8Array): boolean;
  /** Record that it does. */
  markRootHeld?(groupId: Uint8Array): void;
}

/** Lowercase hex, the form every DS path names a journal by. */
export function journalHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  }
  return out;
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function freeQuietly(signer: ProofSigner | null | undefined): void {
  if (signer === null || signer === undefined) return;
  try {
    signer.free();
  } catch {
    /* already freed */
  }
}

/**
 * The per-process registry of journal signers. One instance is the app's
 * (`dsAccess`); tests build their own.
 */
export class DsAccessRegistry {
  private readonly sources = new Set<GroupSignerSource>();
  private readonly groups = new Map<
    string,
    { current: ProofSigner | null; previous: ProofSigner | null }
  >();
  private readonly mailboxes = new Map<string, ProofSigner>();
  private readonly ownerSources = new Set<JournalOwnerSource>();
  private authorityFactory: JournalAuthorityFactory | null = null;
  private readonly claimListeners = new Set<(groupId: Uint8Array) => void>();
  /** ADR-0057 obligation 2 - which journals the owner claimed this session
   *  (`./owner-claim`). Shared by every `DsClient`. */
  readonly ownerClaims = new OwnerClaimMemo();

  /** Add a source; returns the function that removes it. */
  registerGroupSource(source: GroupSignerSource): () => void {
    this.sources.add(source);
    return () => {
      this.sources.delete(source);
    };
  }

  /**
   * Resolve the current signer of `groupId` from the sources and fold it into
   * the cache: a signer for a different epoch or key moves the old current to
   * `previous`. Returns the pair; both stay owned by the registry, so a caller
   * must use them synchronously.
   */
  groupSigners(groupId: Uint8Array): {
    current: ProofSigner | null;
    previous: ProofSigner | null;
  } {
    const hex = journalHex(groupId);
    let fresh: ProofSigner | null = null;
    for (const source of this.sources) {
      let candidate: ProofSigner | null | undefined;
      try {
        candidate = source(groupId);
      } catch {
        candidate = null;
      }
      if (candidate !== null && candidate !== undefined) {
        fresh = candidate;
        break;
      }
    }
    let entry = this.groups.get(hex);
    if (entry === undefined) {
      entry = { current: null, previous: null };
      this.groups.set(hex, entry);
    }
    if (fresh !== null) {
      const current = entry.current;
      if (
        current !== null &&
        current.epoch === fresh.epoch &&
        sameBytes(current.verifyingKey(), fresh.verifyingKey())
      ) {
        freeQuietly(fresh);
      } else {
        if (current !== null) {
          freeQuietly(entry.previous);
          entry.previous = current;
        }
        entry.current = fresh;
      }
    }
    return { current: entry.current, previous: entry.previous };
  }

  /** ADR-0057 - add an owner source; returns the function that removes it. */
  registerOwnerSource(source: JournalOwnerSource): () => void {
    this.ownerSources.add(source);
    return () => {
      this.ownerSources.delete(source);
    };
  }

  /** Install (or clear, with `null`) the signer factory of the signed-in
   *  account. */
  setAuthorityFactory(factory: JournalAuthorityFactory | null): void {
    this.authorityFactory = factory;
  }

  /** `true` when a source says this account owns the journal. */
  ownsJournal(groupId: Uint8Array): boolean {
    for (const source of this.ownerSources) {
      let answer: boolean | null | undefined;
      try {
        answer = source(groupId);
      } catch {
        answer = null;
      }
      if (answer === true) return true;
      if (answer === false) return false;
    }
    return false;
  }

  /**
   * ADR-0057 - what this device may sign for `groupId`: the root when this
   * account owns the journal, else the registrar seed the owner handed it,
   * else `none`. The returned signer is the CALLER's to free
   * (`releaseAuthority`).
   *
   * An owner that is not yet known to hold the root (a transfer whose
   * takeover has not landed, finding 4 of the second security pass) keeps
   * signing with its registrar seed when it has one; with none it signs with
   * the root (a group it founded, the DeviceGroup, a PublisherGroup).
   */
  journalAuthority(groupId: Uint8Array): JournalAuthority {
    const factory = this.authorityFactory;
    if (factory === null) return { kind: "none" };
    try {
      if (this.ownsJournal(groupId)) {
        if (factory.rootHeld !== undefined && !factory.rootHeld(groupId)) {
          const registrar = factory.registrar(groupId);
          if (registrar.kind !== "none") return registrar;
        }
        return factory.root(groupId);
      }
      return factory.registrar(groupId);
    } catch {
      return { kind: "none" };
    }
  }

  /**
   * ADR-0057 obligation 5 / ADR-0050 section 2.5a item 8 - whether this
   * device holds the root or a registrar seed for `groupId`, i.e. whether it
   * could register the epoch a sweep moves to. The one check both the sweep
   * (before staging) and the beacon emitter (before announcing itself) ask:
   * a device that cannot register must neither commit nor make others defer
   * to it.
   */
  mayRegister(groupId: Uint8Array): boolean {
    const authority = this.journalAuthority(groupId);
    try {
      return authority.kind !== "none";
    } finally {
      releaseAuthority(authority);
    }
  }

  /** This account's root over `groupId` whatever the role table says (the
   *  outgoing owner's probe), or `none`. The CALLER frees it. */
  rootAuthority(groupId: Uint8Array): JournalAuthority {
    const factory = this.authorityFactory;
    if (factory === null) return { kind: "none" };
    try {
      return factory.root(groupId);
    } catch {
      return { kind: "none" };
    }
  }

  /** `true` when this account is known to hold the root of `groupId`. */
  rootHeld(groupId: Uint8Array): boolean {
    const factory = this.authorityFactory;
    if (factory?.rootHeld === undefined) return true;
    try {
      return factory.rootHeld(groupId);
    } catch {
      return false;
    }
  }

  /** Record that this account holds the root of `groupId`. Never throws. */
  markRootHeld(groupId: Uint8Array): void {
    try {
      this.authorityFactory?.markRootHeld?.(groupId);
    } catch {
      /* best effort */
    }
  }

  /**
   * ADR-0057 obligation 2 - the owner's claim of `groupId` landed (`204`),
   * lazily after a refused request or on opening the group: mark the root
   * held, settle the session memo, and tell the listeners (the registrar
   * reconcile, the sweep backoff). Never throws.
   */
  noteOwnerClaimed(groupId: Uint8Array): void {
    this.markRootHeld(groupId);
    this.ownerClaims.noteClaimed(groupId);
    for (const listener of this.claimListeners) {
      try {
        listener(groupId);
      } catch {
        /* best effort */
      }
    }
  }

  /** Listen for {@link noteOwnerClaimed}; returns the function that stops. */
  onOwnerClaimed(listener: (groupId: Uint8Array) => void): () => void {
    this.claimListeners.add(listener);
    return () => {
      this.claimListeners.delete(listener);
    };
  }

  /** Install the signer of one of this account's own mailboxes (the account
   *  mailbox and the mailboxes of own predecessor keys; ADR-0078 removed the
   *  device mailbox). Replaces a signer for the same address; the registry
   *  owns it. */
  setMailboxSigner(signer: ProofSigner): string {
    const hex = journalHex(signer.mailboxAddress());
    const prior = this.mailboxes.get(hex);
    if (prior !== undefined && prior !== signer) freeQuietly(prior);
    this.mailboxes.set(hex, signer);
    return hex;
  }

  /** The signer for a mailbox this device owns, or `null` (someone else's
   *  mailbox, a rendezvous mailbox: those stay unsigned). */
  mailboxSigner(pseudonym: Uint8Array): ProofSigner | null {
    return this.mailboxes.get(journalHex(pseudonym)) ?? null;
  }

  /** Drop one mailbox signer (an identity was replaced). */
  dropMailbox(hex: string): void {
    const prior = this.mailboxes.get(hex);
    if (prior !== undefined) freeQuietly(prior);
    this.mailboxes.delete(hex);
  }

  /** Forget everything: sign-out. Frees every held signer. */
  clear(): void {
    for (const entry of this.groups.values()) {
      freeQuietly(entry.current);
      freeQuietly(entry.previous);
    }
    this.groups.clear();
    for (const signer of this.mailboxes.values()) freeQuietly(signer);
    this.mailboxes.clear();
    this.authorityFactory = null;
    this.ownerClaims.clear();
  }
}

/** The app's registry. */
export const dsAccess = new DsAccessRegistry();

// ----------------------------------------------------------------------------
// Refusals and the clock
// ----------------------------------------------------------------------------

/**
 * Parse an HTTP `Date` header into unix milliseconds, or `null` when absent or
 * unparseable.
 */
export function parseHttpDate(value: string | null | undefined): number | null {
  if (value === null || value === undefined || value.trim() === "") return null;
  const ms = Date.parse(value);
  return Number.isFinite(ms) ? ms : null;
}

/** Contract point 5: is a refusal explained by this device's clock? */
export function isClockSkewed(serverDateMs: number | null, nowMs: number): boolean {
  if (serverDateMs === null) return false;
  return Math.abs(serverDateMs - nowMs) > CLOCK_SKEW_LIMIT_MS;
}

/** The shape of the DS client's HTTP error this module reads (structural so
 *  it can walk wrapped causes without importing the client). */
interface RefusalLike {
  readonly status: number;
  readonly clockSkewed?: boolean;
}

/**
 * The user-facing sentence for a failure that was a DS proof refusal caused by
 * a wrong device clock, or `null` for anything else (the caller keeps its
 * ordinary copy). Walks `cause` / `variant.cause` the way `isDsNetworkFailure`
 * does, since both `ChatError` and `ContactsError` wrap the DS error.
 */
export function dsClockSkewNotice(err: unknown): string | null {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (typeof current !== "object") return null;
    const r = current as Partial<RefusalLike> & {
      cause?: unknown;
      variant?: { cause?: unknown };
    };
    if (r.status === 401 && r.clockSkewed === true) return CLOCK_SKEW_NOTICE();
    current = r.cause ?? r.variant?.cause;
  }
  return null;
}

// ----------------------------------------------------------------------------
// Register with the commit (ADR-0053 contract point 3, signed per ADR-0057,
// one request since ADR-0081)
// ----------------------------------------------------------------------------

/** The one network call a commit needs, so the rule below can be tested with
 *  a fake. The DS client implements it by signing the registration with the
 *  journal's authority (root or registrar; `NoJournalAuthorityError` without
 *  one) and sending it with the row (`DsClient.commitGroupEpoch`). */
export interface EpochCommitter {
  commitGroupEpoch(
    groupId: Uint8Array,
    epoch: bigint,
    epochKey: Uint8Array,
    revokeKeys: readonly Uint8Array[],
    row: Uint8Array,
    proof: ProofSigner,
  ): Promise<CommitAnswer>;
}

/**
 * ADR-0081 decision 1 - register the key of the epoch a commit moves to AND
 * append the commit `row`, in one request: `next` is the signer of the new
 * epoch (its verifying key is what is registered), `proofN` the signer of the
 * epoch the commit leaves (it proves the request). Throws when either cannot
 * be derived (a commit whose epoch the server will never know strands every
 * peer that applies it), on a refusal, and with `CommitConflictError` when
 * another commit owns the epoch; the caller then abandons exactly as for a
 * lost race. Resolves to the commit's journal row (an identical re-send of a
 * commit the DS already took answers its original id).
 */
export async function commitWithNextEpoch(
  committer: EpochCommitter,
  groupId: Uint8Array,
  next: ProofSigner | null,
  proofN: ProofSigner | null,
  row: Uint8Array,
  revokeKeys: readonly Uint8Array[] = [],
): Promise<CommitAnswer> {
  if (next === null) {
    throw new Error("ds access: the next epoch's key could not be derived");
  }
  if (proofN === null) {
    throw new Error("ds access: the key of the epoch the commit leaves could not be derived");
  }
  return committer.commitGroupEpoch(groupId, next.epoch, next.verifyingKey(), revokeKeys, row, proofN);
}

/**
 * Derive a group's access signer, or `null` when the group cannot (it is
 * inactive, or the call is refused). Never throws.
 */
export function takeSigner(derive: () => ProofSigner): ProofSigner | null {
  try {
    return derive();
  } catch {
    return null;
  }
}

// ----------------------------------------------------------------------------
// The knock (contract point 6)
// ----------------------------------------------------------------------------

/** What the link joiner's knock needs from the outside world. */
export interface KnockPorts {
  /** `POST /ds/group/<hex>/knock`, one anonymous `ds-knock` token each
   *  (ADR-0054 S3; the DS client retries once on a refused token). An empty
   *  `payloads` is the probe. */
  knock(payloads: readonly Uint8Array[]): Promise<{
    ids: readonly number[];
    epoch: number | null;
  }>;
  /** Build the external-join proposal for the epoch the probe returned. */
  buildProposal(epoch: bigint): Uint8Array;
  /** The advisory join attribution, or `null` when it cannot be built. */
  buildAttribution(): Uint8Array | null;
  /**
   * Make the joiner's state durable: the proposal's KeyPackage private key,
   * minted by {@link buildProposal} into the provider, is the only thing that
   * can open the Welcome the committer sends later. Called after the proposal
   * is built and BEFORE the knock that carries it; a throw aborts the knock,
   * so a refused persist means nothing was published. Required, not a
   * defaulted no-op: a caller that skips it publishes a request whose answer
   * this device may not be able to open after a reload.
   */
  persistBeforeKnock(): Promise<void>;
}

/** The copy for a group that returned no epoch to the probe. */
export const GROUP_NOT_TAKING_REQUESTS = (): string => t("group.join.not_taking_requests");

/** The knock was refused because the group has never been claimed, so no
 *  epoch could be learned. */
export class GroupNotClaimedError extends Error {
  override name = "GroupNotClaimedError";
  constructor() {
    super(GROUP_NOT_TAKING_REQUESTS());
  }
}

/**
 * The link joiner's sequence: probe for the epoch, build the proposal for it,
 * persist the state that minted it, then knock with the attribution and the
 * proposal. No journal read. Each knock spends one anonymous token. The probe
 * publishes nothing, so it may precede the persist.
 */
export async function knockToJoin(ports: KnockPorts): Promise<{
  epoch: bigint;
  ids: readonly number[];
}> {
  const probe = await ports.knock([]);
  if (probe.epoch === null || !Number.isInteger(probe.epoch) || probe.epoch < 0) {
    throw new GroupNotClaimedError();
  }
  const epoch = BigInt(probe.epoch);
  const proposal = ports.buildProposal(epoch);
  const payloads: Uint8Array[] = [];
  const attribution = ports.buildAttribution();
  if (attribution !== null) payloads.push(attribution);
  payloads.push(proposal);
  await ports.persistBeforeKnock();
  const result = await ports.knock(payloads);
  return { epoch, ids: result.ids };
}
