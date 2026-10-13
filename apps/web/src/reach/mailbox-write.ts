// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - choosing the key a write to someone's account mailbox
 * is signed with.
 *
 * Every write to a person's mailbox now needs a key the person registered, or
 * a delegation from one; an unsigned write to a closed mailbox is answered as
 * stored and silently dropped. The order, first match wins:
 *
 * 1. **own** - the mailbox is this account's own (another device of ours):
 *    signed with our own `contact` key, which this account always registers;
 * 2. **explicit** - the caller holds the grant for exactly this write (the
 *    requester's `reply` grant when accepting a contact request, a `/g/`
 *    joiner's `reply` grant, an invite link's `link` grant);
 * 3. **kept** - a grant the person handed us and we kept (contact, then the
 *    group / channel keys of a scope they let find them, then - only for a
 *    Welcome into that group - their `member` key);
 * 4. **delegation** - a committer delegation a proposer posted for this
 *    person, naming our account key: the proof is signed with our account
 *    seed, next to the delegation header;
 * 5. **none** - unsigned. An Everyone (open) or unclaimed mailbox stores it.
 */
import type { ReachGrant as WasmReachGrant } from "../../wasm/lakko_ffi_wasm";
import type { MailboxWriteAuth } from "../ds/client";
import { journalHex, type ProofSigner } from "../ds/access";
import { base64Decode, base64Encode } from "../auth/account-id";
import { acceptedResetOfKey } from "../contacts/local-store";
import { continuityKeyUsable } from "./continuity";
import { loadWasm } from "../wasm";
import { personKeysHex, resolveAccountKey } from "../contacts/address-resolver";
import {
  bestGrantForPerson,
  delegationsForPerson,
  grantsInOrderForPerson,
  type HeldGrant,
} from "./received-grants";
import { reachSession, reachSignerForGrant, reachSignerForGrantWire } from "./reach-service";

/** Which rule authorised a write. */
export type WriteAuthKind = "own" | "explicit" | "kept" | "continuity" | "delegation" | "none";

/**
 * ADR-0073 R.4 - the continuity reach key of an account whose reset this
 * device ACCEPTED: derived from the account id and the attested generation,
 * registered by the resetting device on its new mailbox. A Welcome to the
 * new identity is signed with it when no grant issued by the new identity
 * is held yet.
 */
export interface ContinuityAuth {
  /** The 16-byte account id the reset was accepted for. */
  readonly accountId: Uint8Array;
  /** The accepted generation. */
  readonly keyGeneration: number;
}

/** The pure decision, over what the caller could find. */
export function chooseWriteAuth(facts: {
  readonly isOwnAccount: boolean;
  readonly hasExplicit: boolean;
  readonly kept: HeldGrant | null;
  readonly hasDelegation: boolean;
}): WriteAuthKind {
  if (facts.isOwnAccount) return "own";
  if (facts.hasExplicit) return "explicit";
  if (facts.kept !== null) return "kept";
  if (facts.hasDelegation) return "delegation";
  return "none";
}

/** A resolved authorisation; `release()` frees the signer. */
export interface ResolvedWriteAuth {
  readonly kind: WriteAuthKind;
  readonly auth: MailboxWriteAuth;
  release(): void;
}

function released(kind: WriteAuthKind, signer: ProofSigner | null, delegation: string | null): ResolvedWriteAuth {
  return {
    kind,
    auth: { signer, delegation },
    release: () => {
      try {
        signer?.free();
      } catch {
        /* already freed */
      }
    },
  };
}

/** The account mailbox pseudonym of an account key. */
export async function accountMailboxOf(accountKey: Uint8Array): Promise<Uint8Array> {
  const wasm = await loadWasm();
  // ADR-0074 section 3.5a rule 1 - resolve before you address: a proven
  // predecessor names the person's CURRENT key, and so their current
  // mailbox. An unproven change is never resolved (`address-resolver.ts`).
  return wasm.IdentityPseudonym.fromIdentity(resolveAccountKey(accountKey)).asBytes();
}

/** The mailbox of exactly this key, with no resolution: for READING a
 *  mailbox this device holds the key of, never for addressing a person. */
export async function mailboxOfExactKey(accountKey: Uint8Array): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.IdentityPseudonym.fromIdentity(accountKey).asBytes();
}

/**
 * The key a leaf's Welcome is addressed by: the ACCOUNT key its KeyPackage
 * binds (ADR-0050), or - for a Phase 1 leaf with no binding - the leaf key
 * itself, which is then also the account key.
 */
export async function accountKeyOfKeyPackage(keyPackage: Uint8Array): Promise<Uint8Array> {
  const wasm = await loadWasm();
  return wasm.keyPackageAccountKey(keyPackage) ?? wasm.keyPackageSignatureKey(keyPackage);
}

/**
 * Resolve how to authorise a write to `targetAccountKey`'s mailbox.
 *
 * @param options.scope    the group or channel id the write is about (prefers
 *                         that scope's kept grant; allows the `member` key)
 * @param options.explicit a grant held for exactly this write, as a wasm grant
 *                         (not consumed) or a kept grant wire (base64)
 * @param options.allowMember a Welcome into `scope` may use the `member` key
 */
export async function resolveWriteAuth(
  targetAccountKey: Uint8Array,
  options: {
    readonly scope?: Uint8Array;
    readonly explicit?: WasmReachGrant | string | null;
    readonly allowMember?: boolean;
  } = {},
): Promise<ResolvedWriteAuth> {
  const session = reachSession();
  // ADR-0074 section 3.5a rules 1 and 2: the write goes to the CURRENT key's
  // mailbox (the caller derives it with `accountMailboxOf`, which resolves
  // the same way), and the grants are the person's, under any key they had.
  const personHexes = personKeysHex(journalHex(targetAccountKey));
  const targetHex = personHexes[0] ?? journalHex(targetAccountKey);
  const isOwnAccount = session !== null && session.accountKeyHex === targetHex;
  const kept =
    session === null
      ? null
      : bestGrantForPerson(session.receivedGrants(), personHexes, {
          ...(options.scope === undefined ? {} : { scopeHex: journalHex(options.scope) }),
          allowMember: options.allowMember === true,
        });
  const delegation = session === null ? null : await delegationFor(personHexes);
  const delegationHeader = delegation?.header ?? null;
  const kind = chooseWriteAuth({
    isOwnAccount,
    hasExplicit: options.explicit !== undefined && options.explicit !== null,
    kept,
    hasDelegation: delegationHeader !== null,
  });
  switch (kind) {
    case "own": {
      const grant = session!.grant("contact");
      try {
        return released(kind, await reachSignerForGrant(grant), null);
      } finally {
        grant.free();
      }
    }
    case "explicit": {
      const explicit = options.explicit!;
      const signer =
        typeof explicit === "string"
          ? await reachSignerForGrantWire(explicit)
          : await reachSignerForGrant(explicit);
      return released(kind, signer, null);
    }
    case "kept":
      return released(kind, await reachSignerForGrantWire(kept!.wireB64), null);
    case "delegation":
      return released(kind, await delegation!.signer(), delegationHeader);
    // `chooseWriteAuth` never answers `continuity`: that key is a candidate
    // of `planWriteToPerson` only.
    case "continuity":
    case "none":
      return released("none", null, null);
  }
}

// ----------------------------------------------------------------------------
// A Welcome that must not be written unsigned (ADR-0050 section 2.5a item 7)
// ----------------------------------------------------------------------------

/**
 * No authorisation this account holds reaches the person's mailbox: every
 * kept grant (and delegation) was refused, or there was none. Nothing was
 * written unsigned: an unsigned write to a closed mailbox is answered as
 * stored and silently dropped, which is how a sweep used to count a Welcome
 * delivered that nobody could ever read. The caller keeps the Welcome owed.
 */
export class MailboxUnreachableError extends Error {
  override name = "MailboxUnreachableError";
  constructor(public readonly tried: number) {
    super(
      tried === 0
        ? "no reach grant is held for this mailbox; the write is kept for later"
        : `every reach grant held for this mailbox was refused (${tried}); the write is kept for later`,
    );
  }
}

/** One authorisation to try, resolved lazily so a signer is derived only when
 *  it is about to be used. */
export interface WriteCandidate {
  readonly kind: WriteAuthKind;
  resolve(): Promise<ResolvedWriteAuth>;
}

/** `true` for the DS refusal that means "this key does not open the
 *  mailbox": the uniform `401`. */
export function isWriteRefusal(err: unknown): boolean {
  return err !== null && typeof err === "object" && (err as { status?: unknown }).status === 401;
}

/**
 * Try `candidates` in order, moving to the next only on a `401`; any other
 * failure (offline, a 5xx) is thrown as is, since another key would fare no
 * better. With none left, throws {@link MailboxUnreachableError}; with none at
 * all, nothing is written. Each signer is released after its attempt.
 */
export async function writeThroughCandidates<T>(
  candidates: readonly WriteCandidate[],
  write: (auth: MailboxWriteAuth, kind: WriteAuthKind) => Promise<T>,
): Promise<T> {
  let tried = 0;
  for (const candidate of candidates) {
    const resolved = await candidate.resolve();
    tried += 1;
    try {
      return await write(resolved.auth, resolved.kind);
    } catch (err) {
      if (!isWriteRefusal(err)) throw err;
    } finally {
      resolved.release();
    }
  }
  throw new MailboxUnreachableError(tried);
}

// ----------------------------------------------------------------------------
// A refused write re-checks the address once (ADR-0074 section 3.5a rule 6)
// ----------------------------------------------------------------------------

/** Where a write goes and what it may be signed with, as resolved NOW. */
export interface WritePlan {
  /** The mailbox pseudonym of the person's current key, as this device
   *  knows it. */
  readonly mailbox: Uint8Array;
  readonly candidates: readonly WriteCandidate[];
}

/**
 * Write through a plan, re-checking the address ONCE when the mailbox
 * refuses (rule 6).
 *
 * A `401` means "this key does not open this mailbox". Either the grant is
 * dead, or the ADDRESS is: the person rotated their account keys, their
 * re-sent profile reached this device before the proven change did, and the
 * new grant is being offered to the mailbox that was left. So on the first
 * refusal `recheck` runs (it fetches the person's identity bundle through
 * the fail-closed gate, which records a proven predecessor), the plan is
 * resolved AGAIN (a new address, the same grants in a new order) and every
 * candidate of that plan is tried, moving on only on `401`.
 *
 *   - any failure other than `401` is thrown as is, at once;
 *   - `recheck` runs at most once per write, and a failing one is ignored
 *     (the second plan is then the first one, minus nothing);
 *   - with `recheck` `null` the candidates are simply tried in order;
 *   - with nothing left: the last refusal, or {@link MailboxUnreachableError}
 *     when `unreachable` is asked for (a Welcome, which then stays owed).
 *
 * Each signer is released after its attempt.
 */
export async function writeWithAddressRecheck<T>(
  plan: () => Promise<WritePlan>,
  write: (mailbox: Uint8Array, auth: MailboxWriteAuth, kind: WriteAuthKind) => Promise<T>,
  options: {
    readonly recheck: (() => Promise<void>) | null;
    /** Throw {@link MailboxUnreachableError} rather than the refusal. */
    readonly unreachable?: boolean;
  },
): Promise<T> {
  let rechecked = options.recheck === null;
  let current = await plan();
  for (;;) {
    let refusal: unknown = null;
    let tried = 0;
    let again = false;
    for (const candidate of current.candidates) {
      const resolved = await candidate.resolve();
      tried += 1;
      try {
        return await write(current.mailbox, resolved.auth, resolved.kind);
      } catch (err) {
        if (!isWriteRefusal(err)) throw err;
        refusal = err;
      } finally {
        resolved.release();
      }
      if (!rechecked) {
        rechecked = true;
        try {
          await options.recheck?.();
        } catch {
          /* the address could not be re-checked: the plan stands */
        }
        current = await plan();
        again = true;
        break;
      }
    }
    if (again) continue;
    if (options.unreachable === true || refusal === null) throw new MailboxUnreachableError(tried);
    throw refusal;
  }
}

/**
 * The plan for a write to the person `targetAccountKey` names: their CURRENT
 * mailbox (rule 1) and, in order,
 *
 *   - this account's own mailbox: its own `contact` key, alone;
 *   - an explicit grant for exactly this write, alone;
 *   - otherwise every grant kept for the person under any key they have had
 *     (rule 2), the ones of before a rotation last and only while the old
 *     address stands (rule 6), then a committer delegation;
 *   - and, with none of those and unless `neverUnsigned`, the unsigned write
 *     (which only an Everyone mailbox stores).
 */
export async function planWriteToPerson(
  targetAccountKey: Uint8Array,
  options: {
    readonly scope?: Uint8Array;
    readonly explicit?: WasmReachGrant | string | null;
    readonly allowMember?: boolean;
    readonly neverUnsigned?: boolean;
    /** ADR-0073 R.4: tried after every grant the person itself issued (once
     *  the new identity is in a conversation its own grants are used and
     *  this key is not), before a committer delegation. */
    readonly continuity?: ContinuityAuth | null;
  } = {},
): Promise<WritePlan> {
  // A Welcome (`allowMember`) to the new identity of an account whose reset
  // this device accepted resolves the continuity key by itself, so every
  // Welcome path (a direct add, the sweep, the outbox replay) signs alike.
  const continuity =
    options.continuity !== undefined
      ? options.continuity
      : options.allowMember === true
        ? continuityFor(targetAccountKey, Date.now())
        : null;
  const session = reachSession();
  const mailbox = await accountMailboxOf(targetAccountKey);
  const personHexes = personKeysHex(journalHex(targetAccountKey));
  const targetHex = personHexes[0] ?? journalHex(targetAccountKey);
  const isOwnAccount = session !== null && session.accountKeyHex === targetHex;
  const explicit = options.explicit ?? null;
  if (isOwnAccount) {
    return {
      mailbox,
      candidates: [
        {
          kind: "own",
          resolve: async () => {
            const grant = session.grant("contact");
            try {
              return released("own", await reachSignerForGrant(grant), null);
            } finally {
              grant.free();
            }
          },
        },
      ],
    };
  }
  if (explicit !== null) {
    return {
      mailbox,
      candidates: [
        {
          kind: "explicit",
          resolve: async () =>
            released(
              "explicit",
              typeof explicit === "string"
                ? await reachSignerForGrantWire(explicit)
                : await reachSignerForGrant(explicit),
              null,
            ),
        },
      ],
    };
  }
  const candidates: WriteCandidate[] = [];
  if (session !== null) {
    // ADR-0073 R.7 case 3 - a Welcome into the successor of a conversation
    // may use the `member` key the person granted for the conversation's
    // EARLIER group: the lineage makes it the same conversation. The scope
    // itself is tried first; each grant once.
    const scopeHexes =
      options.scope === undefined
        ? [undefined]
        : [journalHex(options.scope), ...scopeLineage(journalHex(options.scope))];
    const offered = new Set<string>();
    for (const scopeHex of scopeHexes) {
      for (const held of grantsInOrderForPerson(session.receivedGrants(), personHexes, {
        ...(scopeHex === undefined ? {} : { scopeHex }),
        allowMember: options.allowMember === true,
      })) {
        if (offered.has(held.wireB64)) continue;
        offered.add(held.wireB64);
        candidates.push({
          kind: "kept",
          resolve: async () => released("kept", await reachSignerForGrantWire(held.wireB64), null),
        });
      }
    }
    const delegation = await delegationFor(personHexes);
    if (continuity !== null) candidates.push(continuityCandidate(continuity));
    if (delegation !== null) {
      candidates.push({
        kind: "delegation",
        resolve: async () => released("delegation", await delegation.signer(), delegation.header),
      });
    }
  }
  // With no reach session (nothing kept, no delegation) the continuity key is
  // still derivable: it needs nothing this account holds.
  if (session === null && continuity !== null) candidates.push(continuityCandidate(continuity));
  if (candidates.length === 0 && options.neverUnsigned !== true) {
    candidates.push({ kind: "none", resolve: () => Promise.resolve(released("none", null, null)) });
  }
  return { mailbox, candidates };
}

/**
 * The continuity key a Welcome to `targetAccountKey` may be signed with
 * (R.4), or `null`: only when that key is the one an accepted reset re-pinned
 * this device to, and only inside the key's window, counted from the
 * attestation's `signed_at` this device verified. Reads the pin store;
 * never throws.
 */
export function continuityFor(targetAccountKey: Uint8Array, nowMs: number): ContinuityAuth | null {
  try {
    const reset = acceptedResetOfKey(base64Encode(targetAccountKey));
    if (reset === null || !continuityKeyUsable(reset.signedAtMs, nowMs)) return null;
    return { accountId: base64Decode(reset.accountIdB64), keyGeneration: reset.keyGeneration };
  } catch {
    return null;
  }
}

/** The continuity key as one write candidate; the seed is derived only when
 *  the candidate is about to be tried, and wiped after. */
function continuityCandidate(continuity: ContinuityAuth): WriteCandidate {
  return {
    kind: "continuity",
    resolve: async () => {
      const wasm = await loadWasm();
      const seed = wasm.continuityReachSeed(continuity.accountId, BigInt(continuity.keyGeneration));
      try {
        return released("continuity", wasm.DsProofSigner.forReach(seed), null);
      } finally {
        seed.fill(0);
      }
    },
  };
}

/** The earlier groups of the conversation a scope continues, as hex, or
 *  nothing (ADR-0073 R.5). Installed by the runtime that holds the lineage. */
export type ScopeLineage = (scopeHex: string) => readonly string[];

let installedScopeLineage: ScopeLineage | null = null;

/** Install (or, with `null`, remove) the lineage a Welcome's grant scope is
 *  widened through. */
export function installScopeLineage(lineage: ScopeLineage | null): void {
  installedScopeLineage = lineage;
}

function scopeLineage(scopeHex: string): readonly string[] {
  try {
    return (installedScopeLineage?.(scopeHex) ?? []).filter((hex) => hex !== scopeHex);
  } catch {
    return [];
  }
}

/** Re-checks the address of the person a key names (rule 6): fetch their
 *  identity bundle through the fail-closed gate. Never throws for a refusal
 *  the caller cannot act on. */
export type AddressRecheck = (accountKey: Uint8Array) => Promise<void>;

let installedRecheck: AddressRecheck | null = null;

/** Install the app's address re-check (the mailbox provider's, which holds
 *  the directory client and the gate); `null` removes it. Every write to a
 *  person's mailbox that does not name its own uses this one. */
export function installAddressRecheck(recheck: AddressRecheck | null): void {
  installedRecheck = recheck;
}

/**
 * A write to a person's account mailbox: resolved (rule 1), authorised with
 * what is held for the person (rule 2), and re-checked once on a refusal
 * (rule 6). `write` receives the mailbox to write to.
 */
export async function writeToPerson<T>(
  targetAccountKey: Uint8Array,
  options: {
    readonly scope?: Uint8Array;
    readonly explicit?: WasmReachGrant | string | null;
    readonly allowMember?: boolean;
    /** A Welcome: never written unsigned, and an exhausted write throws
     *  {@link MailboxUnreachableError} so the caller keeps it owed. */
    readonly neverUnsigned?: boolean;
    readonly recheck?: AddressRecheck | null;
    /** ADR-0073 R.4: see {@link planWriteToPerson}. */
    readonly continuity?: ContinuityAuth | null;
  },
  write: (mailbox: Uint8Array, auth: MailboxWriteAuth, kind: WriteAuthKind) => Promise<T>,
): Promise<T> {
  const recheck = options.recheck === undefined ? installedRecheck : options.recheck;
  return writeWithAddressRecheck(() => planWriteToPerson(targetAccountKey, options), write, {
    recheck: recheck === null ? null : () => recheck(targetAccountKey),
    unreachable: options.neverUnsigned === true,
  });
}

/**
 * A Welcome write to `targetAccountKey`'s account mailbox that is never sent
 * unsigned (ADR-0050 section 2.5a item 7):
 *
 *   - this account's own mailbox and an explicit grant: one candidate;
 *   - otherwise every kept grant for that account in {@link resolveWriteAuth}'s
 *     order (contact, the scopes' `group`, their `channel`, then this group's
 *     `member`), then a committer delegation naming this account, moving to
 *     the next on `401`;
 *   - with nothing left, {@link MailboxUnreachableError} and no write.
 *
 * The web client cannot tell whether a person's mailbox is open (Everyone):
 * the DS answers an unsigned write to an open and to a closed mailbox alike.
 * So a Welcome for an account this device holds no grant for waits, owed,
 * until one arrives; it is never written unsigned.
 *
 * ADR-0074 section 3.5a rule 6: with `recheck`, the first refusal re-checks
 * the person's address once. `write` receives the mailbox to write to.
 */
export async function writeWelcome<T>(
  targetAccountKey: Uint8Array,
  options: {
    readonly scope: Uint8Array;
    readonly explicit?: WasmReachGrant | string | null;
    readonly recheck?: AddressRecheck | null;
  },
  write: (mailbox: Uint8Array, auth: MailboxWriteAuth, kind: WriteAuthKind) => Promise<T>,
): Promise<T> {
  return writeToPerson(
    targetAccountKey,
    {
      scope: options.scope,
      explicit: options.explicit ?? null,
      allowMember: true,
      neverUnsigned: true,
      recheck: options.recheck ?? null,
    },
    write,
  );
}

/**
 * The delegation a kept committer-delegation envelope holds for one of this
 * account's keys, for an add of the person `addedHexes` names (every key
 * they have had), with the signer of the key it names.
 *
 * ADR-0074 section 3.5a: a proposer names the committer's account key as IT
 * knows it. One that has observed this account's proven rotation names the
 * current key; one that has not names the key the role table carries, the
 * predecessor. So each own key this device holds the seed of is tried, the
 * current one first, and the write is signed with the key that was named.
 */
async function delegationFor(
  addedHexes: readonly string[],
): Promise<{ readonly header: string; signer(): Promise<ProofSigner> } | null> {
  const session = reachSession();
  if (session === null) return null;
  const envelopes = delegationsForPerson(session.receivedGrants(), addedHexes);
  if (envelopes.length === 0) return null;
  const wasm = await loadWasm();
  const delegates = session.delegateKeys();
  for (const held of envelopes) {
    let env;
    try {
      env = wasm.decodeCommitterDelegationEnvelope(base64Decode(held.envelopeB64));
    } catch {
      continue;
    }
    try {
      for (const delegate of delegates) {
        const header = wasm.committerDelegationFor(env, delegate.accountKey);
        if (header !== undefined) return { header, signer: () => delegate.signer() };
      }
    } catch {
      /* a malformed envelope authorises nothing */
    } finally {
      env.free();
    }
  }
  return null;
}

/**
 * The account key a leaf belongs to, read off a roster (`Group.members()`):
 * the leaf's ADR-0050 `accountKey`, or the leaf key itself for a Phase 1
 * leaf or a leaf the roster does not hold.
 */
export function accountKeyOfLeaf(
  roster: readonly { readonly signaturePublicKey: Uint8Array; readonly accountKey?: Uint8Array }[],
  leafKey: Uint8Array,
): Uint8Array {
  const hex = journalHex(leafKey);
  for (const m of roster) {
    if (journalHex(m.signaturePublicKey) === hex) return m.accountKey ?? leafKey;
  }
  return leafKey;
}
