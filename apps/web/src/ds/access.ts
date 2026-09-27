// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 - a journal on the Delivery Service is opened by a key, not by its
 * name. This module holds the client half of that contract that is not a
 * network call: which signer proves a request to which journal, the rules the
 * DS client follows when a proof is refused, and the pure decisions the call
 * sites share (register-before-publish, heal-on-apply, the clock-skew copy,
 * the knock sequence).
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

/** The header a proof travels in (`dsProofHeaderName()` on the wasm surface;
 *  pinned against it by `access.test.ts`). */
export const DS_PROOF_HEADER = "x-lakko-ds-proof";

/** Contract point 5: a refusal whose response `Date` differs from the device
 *  clock by more than this is reported as a clock problem. */
export const CLOCK_SKEW_LIMIT_MS = 60_000;

/** The copy a user sees when a refusal is explained by the device clock. */
export const CLOCK_SKEW_NOTICE = "Check this device's date and time.";

/**
 * The part of the wasm `DsProofSigner` this client uses. Structural, so the
 * host tests can drive the rules with a fake.
 */
export interface ProofSigner {
  readonly epoch: bigint;
  sign(method: string, path: string, query: string, body: Uint8Array): string;
  groupClaimBodyJson(): string;
  mailboxClaimBodyJson(): string;
  verifyingKey(): Uint8Array;
  mailboxPseudonym(): Uint8Array;
  free(): void;
}

/** Derive the signer of the epoch a group is at now, or `null` when this
 *  source does not hold the group (or cannot derive: an evicted group). The
 *  returned signer is handed over: the registry frees it. */
export type GroupSignerSource = (groupId: Uint8Array) => ProofSigner | null | undefined;

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
  private readonly claimedGroups = new Set<string>();

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

  /** `true` the first time a group is seen in this session; the DS client
   *  claims it then (contract point 2, claim on open). */
  markClaimAttempt(groupId: Uint8Array): boolean {
    const hex = journalHex(groupId);
    if (this.claimedGroups.has(hex)) return false;
    this.claimedGroups.add(hex);
    return true;
  }

  /** Install the signer of one of this device's own mailboxes (the account
   *  mailbox and the device mailbox). Replaces a signer for the same
   *  pseudonym; the registry owns it. */
  setMailboxSigner(signer: ProofSigner): string {
    const hex = journalHex(signer.mailboxPseudonym());
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
    this.claimedGroups.clear();
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
    if (r.status === 401 && r.clockSkewed === true) return CLOCK_SKEW_NOTICE;
    current = r.cause ?? r.variant?.cause;
  }
  return null;
}

// ----------------------------------------------------------------------------
// Register before publish, heal on apply
// ----------------------------------------------------------------------------

/** The one network call the two rules below need, so they can be tested with
 *  a fake. `signedBy === null` sends the registration unsigned. */
export interface EpochRegistrar {
  registerGroupEpoch(
    groupId: Uint8Array,
    next: ProofSigner,
    signedBy: ProofSigner | null,
  ): Promise<void>;
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

/**
 * Contract point 3: register the key of the epoch a commit moves to, proven
 * under the key of the epoch it leaves, BEFORE the commit is published. Throws
 * when the registration is refused; the caller aborts exactly as it would for
 * a failed outbox write (no publish).
 *
 * `next === null` means the new epoch's signer could not be derived, which is
 * itself a reason to abort: a commit whose epoch the server will never know
 * strands every peer that applies it.
 */
export async function registerBeforePublish(
  registrar: EpochRegistrar,
  groupId: Uint8Array,
  signerN: ProofSigner | null,
  signerN1: ProofSigner | null,
): Promise<void> {
  if (signerN1 === null) {
    throw new Error("ds access: the next epoch's key could not be derived");
  }
  await registrar.registerGroupEpoch(groupId, signerN1, signerN);
}

/**
 * Contract point 4: after a receive batch, when the epoch moved, register the
 * new epoch's key proven under the key captured BEFORE the batch. Best effort:
 * a refusal is expected (someone registered first, or this device was too far
 * behind) and ignored. Returns whether a registration was attempted.
 */
export async function healAfterApply(
  registrar: EpochRegistrar,
  groupId: Uint8Array,
  before: ProofSigner | null,
  after: ProofSigner | null,
): Promise<boolean> {
  if (before === null || after === null) return false;
  if (after.epoch === before.epoch && sameBytes(after.verifyingKey(), before.verifyingKey())) {
    return false;
  }
  try {
    await registrar.registerGroupEpoch(groupId, after, before);
  } catch {
    /* expected when another member registered first */
  }
  return true;
}

// ----------------------------------------------------------------------------
// The knock (contract point 6)
// ----------------------------------------------------------------------------

/** A proof-of-work proof, as the DS takes it. */
export interface KnockPow {
  readonly seed: string;
  readonly nonce: string;
  readonly solution: string;
}

/** What the link joiner's knock needs from the outside world. */
export interface KnockPorts {
  /** `POST /pow/challenge` on the DS. */
  challenge(): Promise<{ seed: string; effortBits: number }>;
  /** The client's PoW solver. */
  solve(seed: string, effortBits: number): Promise<KnockPow>;
  /** `POST /ds/group/<hex>/knock`. An empty `payloads` is the probe. */
  knock(pow: KnockPow, payloads: readonly Uint8Array[]): Promise<{
    ids: readonly number[];
    epoch: number | null;
  }>;
  /** Build the external-join proposal for the epoch the probe returned. */
  buildProposal(epoch: bigint): Uint8Array;
  /** The advisory join attribution, or `null` when it cannot be built. */
  buildAttribution(): Uint8Array | null;
}

/** The knock was refused because the group has never been claimed, so no
 *  epoch could be learned. */
export class GroupNotClaimedError extends Error {
  override name = "GroupNotClaimedError";
  constructor() {
    super("This group is not taking requests yet. Try again later.");
  }
}

/**
 * The link joiner's sequence: probe for the epoch, build the proposal for it,
 * then knock with the attribution and the proposal. No journal read. Two
 * separate proofs of work, because the server consumes a challenge on use.
 */
export async function knockToJoin(ports: KnockPorts): Promise<{
  epoch: bigint;
  ids: readonly number[];
}> {
  const probeChallenge = await ports.challenge();
  const probePow = await ports.solve(probeChallenge.seed, probeChallenge.effortBits);
  const probe = await ports.knock(probePow, []);
  if (probe.epoch === null || !Number.isInteger(probe.epoch) || probe.epoch < 0) {
    throw new GroupNotClaimedError();
  }
  const epoch = BigInt(probe.epoch);
  const proposal = ports.buildProposal(epoch);
  const payloads: Uint8Array[] = [];
  const attribution = ports.buildAttribution();
  if (attribution !== null) payloads.push(attribution);
  payloads.push(proposal);
  const challenge = await ports.challenge();
  const pow = await ports.solve(challenge.seed, challenge.effortBits);
  const result = await ports.knock(pow, payloads);
  return { epoch, ids: result.ids };
}
