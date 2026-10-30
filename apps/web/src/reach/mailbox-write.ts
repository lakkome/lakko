// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - choosing the key a write to someone's account mailbox
 * is signed with; ADR-0078 phase 1 - and WHERE it goes.
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
 * 5. **none** - unsigned, spending one `mailbox-write` token. An Everyone
 *    (open) or unclaimed mailbox stores it.
 *
 * ADR-0078: a mailbox address is no longer a function of the person's
 * identity key. It is `mailboxAddress(mailbox_pk)` for a mailbox key the
 * person derives from their account seed, and a writer learns it only from a
 * MAILBOX BINDING (the mailbox key signed by the person's identity key) that
 * travels with every grant, link, introduction and public record. So each
 * candidate carries its own target: before the write the binding is verified
 * against the account key this device holds pinned for the person (the key
 * itself and the proven predecessors of its chain), the row is sealed to the
 * verified key and addressed to its address. A binding that does not verify
 * refuses that candidate (fail closed; logged without identifiers). Nothing
 * here derives an address from an identity key or an account id.
 */
import type { ReachGrant as WasmReachGrant } from "../../wasm/lakko_ffi_wasm";
import type { MailboxWriteAuth } from "../ds/client";
import { journalHex, type ProofSigner } from "../ds/access";
import { base64Decode, base64Encode } from "../auth/account-id";
import { acceptedResetOfKey } from "../contacts/local-store";
import type { MailboxTarget } from "../contacts/mailbox-row";
import { continuityKeyUsable } from "./continuity";
import { loadWasm } from "../wasm";
import { personKeysHex } from "../contacts/address-resolver";
import {
  delegationsForPerson,
  grantsInOrderForPerson,
  type HeldGrant,
} from "./received-grants";
import { reachSession, reachSignerForGrant, reachSignerForGrantWire } from "./reach-service";

export type { MailboxTarget } from "../contacts/mailbox-row";

/** Which rule authorised a write. */
export type WriteAuthKind = "own" | "explicit" | "kept" | "continuity" | "delegation" | "none";

/**
 * ADR-0073 R.4 - the continuity reach key of an account whose reset this
 * device ACCEPTED: derived from the account id and the attested generation.
 * ADR-0078: the resetting device no longer registers it on its new mailbox.
 *
 * ADR-0078 open question: a Welcome under this key needs the reset account's
 * NEW mailbox, and no carrier names it (the address is no longer derivable
 * from the account id or the identity key). Until one exists the write is a
 * recorded no-op (see {@link planWriteToPerson}).
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

/** A resolved authorisation and where it writes; `release()` frees the
 *  signer. */
export interface ResolvedWriteAuth {
  readonly kind: WriteAuthKind;
  /** The verified mailbox key the row is sealed to, and its address. */
  readonly target: MailboxTarget;
  readonly auth: MailboxWriteAuth;
  release(): void;
}

function released(
  kind: WriteAuthKind,
  target: MailboxTarget,
  signer: ProofSigner | null,
  delegation: string | null,
): ResolvedWriteAuth {
  return {
    kind,
    target,
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

// ----------------------------------------------------------------------------
// The mailbox binding (ADR-0078)
// ----------------------------------------------------------------------------

/** Log a refused binding. No identifier of any kind is logged. */
function noteRefusedBinding(kind: WriteAuthKind): void {
  // eslint-disable-next-line no-console
  console.warn(`ADR-0078: a ${kind} mailbox write was refused; its mailbox binding did not verify`);
}

/** The person's keys as bytes: `targetAccountKey` itself first, then the
 *  proven keys of their chain (`personKeysHex`), each once. With `exact`,
 *  the given key alone. */
export function personKeysOf(targetAccountKey: Uint8Array, exact = false): Uint8Array[] {
  if (exact) return [targetAccountKey];
  const out: Uint8Array[] = [targetAccountKey];
  const seen = new Set<string>([journalHex(targetAccountKey)]);
  for (const hex of personKeysHex(journalHex(targetAccountKey))) {
    if (seen.has(hex)) continue;
    seen.add(hex);
    const bytes = new Uint8Array(hex.length / 2);
    for (let i = 0; i < bytes.length; i++) bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    out.push(bytes);
  }
  return out;
}

/**
 * The target a grant writes to: its binding verified against one of the
 * person's pinned keys (`personKeys`), or `null` when it verifies against
 * none (or the grant does not decode). Never throws.
 */
export async function verifiedTargetOfGrant(
  grant: WasmReachGrant | string,
  personKeys: readonly Uint8Array[],
): Promise<MailboxTarget | null> {
  const wasm = await loadWasm();
  let decoded: WasmReachGrant;
  let owned = false;
  if (typeof grant === "string") {
    try {
      decoded = wasm.decodeReachGrant(base64Decode(grant));
      owned = true;
    } catch {
      return null;
    }
  } else {
    decoded = grant;
  }
  try {
    for (const key of personKeys) {
      try {
        const mailboxPk = decoded.mailboxPkVerified(key);
        return { mailboxPk, address: wasm.mailboxAddress(mailboxPk) };
      } catch {
        /* not this key */
      }
    }
    return null;
  } finally {
    if (owned) {
      try {
        decoded.free();
      } catch {
        /* already freed */
      }
    }
  }
}

/** The target a bare 97-byte binding names, verified against one of the
 *  person's pinned keys, or `null`. Never throws. */
export async function verifiedTargetOfBinding(
  binding: Uint8Array,
  personKeys: readonly Uint8Array[],
): Promise<MailboxTarget | null> {
  const wasm = await loadWasm();
  for (const key of personKeys) {
    try {
      const mailboxPk = wasm.verifyMailboxBinding(binding, key);
      return { mailboxPk, address: wasm.mailboxAddress(mailboxPk) };
    } catch {
      /* not this key */
    }
  }
  return null;
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

// ----------------------------------------------------------------------------
// A Welcome that must not be written unsigned (ADR-0050 section 2.5a item 7)
// ----------------------------------------------------------------------------

/**
 * No authorisation this account holds reaches the person's mailbox: every
 * kept grant (and delegation) was refused, or there was none, or no binding
 * verified. Nothing was written unsigned: an unsigned write to a closed
 * mailbox is answered as stored and silently dropped, which is how a sweep
 * used to count a Welcome delivered that nobody could ever read. The caller
 * keeps the Welcome owed.
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

/** One authorisation to try, resolved lazily so a signer is derived (and a
 *  binding verified) only when it is about to be used. `null`: the
 *  candidate's mailbox binding did not verify, and it is skipped. */
export interface WriteCandidate {
  readonly kind: WriteAuthKind;
  resolve(): Promise<ResolvedWriteAuth | null>;
}

/** `true` for the DS refusal that means "this key does not open the
 *  mailbox": the uniform `401`. */
export function isWriteRefusal(err: unknown): boolean {
  return err !== null && typeof err === "object" && (err as { status?: unknown }).status === 401;
}

/** The write a candidate performs: to `target`, under `auth`. */
export type MailboxWrite<T> = (target: MailboxTarget, auth: MailboxWriteAuth, kind: WriteAuthKind) => Promise<T>;

/**
 * Try `candidates` in order, moving to the next only on a `401` (or when a
 * candidate's binding does not verify); any other failure (offline, a 5xx) is
 * thrown as is, since another key would fare no better. With none left,
 * throws {@link MailboxUnreachableError}; with none at all, nothing is
 * written. Each signer is released after its attempt.
 */
export async function writeThroughCandidates<T>(
  candidates: readonly WriteCandidate[],
  write: MailboxWrite<T>,
): Promise<T> {
  let tried = 0;
  for (const candidate of candidates) {
    const resolved = await candidate.resolve();
    if (resolved === null) continue;
    tried += 1;
    try {
      return await write(resolved.target, resolved.auth, resolved.kind);
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

/** What a write may be signed with, as resolved NOW; each candidate carries
 *  the mailbox its binding names. */
export interface WritePlan {
  readonly candidates: readonly WriteCandidate[];
}

/**
 * Write through a plan, re-checking the address ONCE when the mailbox
 * refuses (rule 6).
 *
 * A `401` means "this key does not open this mailbox". Either the grant is
 * dead, or the ADDRESS is: the person rotated their account keys, their
 * re-sent profile reached this device before the proven change did, and the
 * new grant is being offered under a key this device does not yet pin. So on
 * the first refusal `recheck` runs (it fetches the person's identity bundle
 * through the fail-closed gate, which records a proven predecessor), the plan
 * is resolved AGAIN (the same grants, verified against the keys now pinned,
 * in a new order) and every candidate of that plan is tried, moving on only
 * on `401`.
 *
 *   - any failure other than `401` is thrown as is, at once;
 *   - `recheck` runs at most once per write, and a failing one is ignored
 *     (the second plan is then the first one, minus nothing);
 *   - with `recheck` `null` the candidates are simply tried in order;
 *   - a candidate whose binding does not verify is skipped, never written;
 *   - with nothing left: the last refusal, or {@link MailboxUnreachableError}
 *     when `unreachable` is asked for (a Welcome, which then stays owed).
 *
 * Each signer is released after its attempt.
 */
export async function writeWithAddressRecheck<T>(
  plan: () => Promise<WritePlan>,
  write: MailboxWrite<T>,
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
      if (resolved === null) continue;
      tried += 1;
      try {
        return await write(resolved.target, resolved.auth, resolved.kind);
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

/** A candidate over one grant: verified against `personKeys` when resolved. */
function grantCandidate(
  kind: WriteAuthKind,
  grant: WasmReachGrant | string,
  personKeys: readonly Uint8Array[],
): WriteCandidate {
  return {
    kind,
    resolve: async () => {
      const target = await verifiedTargetOfGrant(grant, personKeys);
      if (target === null) {
        noteRefusedBinding(kind);
        return null;
      }
      const signer =
        typeof grant === "string" ? await reachSignerForGrantWire(grant) : await reachSignerForGrant(grant);
      return released(kind, target, signer, null);
    },
  };
}

/**
 * The plan for a write to the person `targetAccountKey` names, in order:
 *
 *   - this account's own mailbox: its own `contact` key, alone;
 *   - an explicit grant for exactly this write, alone;
 *   - otherwise every grant kept for the person under any key they have had
 *     (rule 2), the ones of before a rotation last and only while the old
 *     address stands (rule 6), then a committer delegation;
 *   - with none of those, for a Welcome (`openWelcome`), the unsigned write
 *     to the binding a kept no-delegation committer envelope names for this
 *     person in `scope` (ADR-0060 section 6, a person found in Everyone);
 *   - and, with none of those, unless `neverUnsigned`, and only when the
 *     caller holds the person's mailbox binding (`openBinding`: from the
 *     public record of a person in Everyone), the unsigned write.
 *
 * Every candidate's binding is verified against the person's pinned keys
 * (`exactKey`: against `targetAccountKey` alone) when it is resolved.
 */
export async function planWriteToPerson(
  targetAccountKey: Uint8Array,
  options: {
    readonly scope?: Uint8Array;
    readonly explicit?: WasmReachGrant | string | null;
    readonly allowMember?: boolean;
    readonly neverUnsigned?: boolean;
    /** The person's 97-byte mailbox binding from a public record, for the
     *  unsigned write to an open mailbox. */
    readonly openBinding?: Uint8Array | null;
    /** Verify bindings against `targetAccountKey` alone (a contact request is
     *  sealed to the keys in hand). */
    readonly exactKey?: boolean;
    /** ADR-0073 R.4 / ADR-0078 open question: see {@link ContinuityAuth}. */
    readonly continuity?: ContinuityAuth | null;
    /**
     * A sweep's Welcome (ADR-0060 section 6, a person found in Everyone):
     * when no signed candidate exists, and a kept committer-delegation
     * envelope with NO delegation names this person for `scope`, the Welcome
     * is written unsigned to the mailbox that envelope's binding names
     * (verified against the person's keys). Never for anyone else. Needs
     * `scope`.
     */
    readonly openWelcome?: boolean;
  } = {},
): Promise<WritePlan> {
  // A Welcome (`allowMember`) to the new identity of an account whose reset
  // this device accepted used to be signed with the continuity key and
  // addressed to a mailbox derived from the identity key. ADR-0078 open
  // question: no carrier names the reset account's new mailbox, so that
  // write is a recorded no-op. The kept grants of the new identity (once it
  // re-sends its profile) still reach it.
  const continuity =
    options.continuity !== undefined
      ? options.continuity
      : options.allowMember === true
        ? continuityFor(targetAccountKey, Date.now())
        : null;
  if (continuity !== null) noteContinuitySkipped();
  const session = reachSession();
  const personKeys = personKeysOf(targetAccountKey, options.exactKey === true);
  const personHexes = personKeysHex(journalHex(targetAccountKey));
  const targetHex = personHexes[0] ?? journalHex(targetAccountKey);
  const isOwnAccount = session !== null && session.accountKeyHex === targetHex;
  const explicit = options.explicit ?? null;
  if (isOwnAccount) {
    return {
      candidates: [
        {
          kind: "own",
          resolve: async () => {
            const grant = session.grant("contact");
            try {
              const target = await verifiedTargetOfGrant(grant, [session.accountKey]);
              if (target === null) {
                noteRefusedBinding("own");
                return null;
              }
              return released("own", target, await reachSignerForGrant(grant), null);
            } finally {
              grant.free();
            }
          },
        },
      ],
    };
  }
  if (explicit !== null) return { candidates: [grantCandidate("explicit", explicit, personKeys)] };
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
        candidates.push(grantCandidate("kept", held.wireB64, personKeys));
      }
    }
    const delegation = await delegationFor(personHexes);
    if (delegation !== null) {
      candidates.push({
        kind: "delegation",
        resolve: async () => {
          const target = await verifiedTargetOfBinding(delegation.binding, personKeys);
          if (target === null) {
            noteRefusedBinding("delegation");
            return null;
          }
          return released("delegation", target, await delegation.signer(), delegation.header);
        },
      });
    }
  }
  if (candidates.length === 0 && options.openWelcome === true && options.scope !== undefined && session !== null) {
    const binding = await openWelcomeBinding(personHexes, journalHex(options.scope));
    if (binding !== null) candidates.push(unsignedCandidate(binding, personKeys));
  }
  const openBinding = options.openBinding ?? null;
  if (candidates.length === 0 && options.neverUnsigned !== true && openBinding !== null) {
    candidates.push(unsignedCandidate(openBinding, personKeys));
  }
  return { candidates };
}

/** The unsigned candidate to the mailbox `binding` names, verified against
 *  `personKeys` when resolved. */
function unsignedCandidate(binding: Uint8Array, personKeys: readonly Uint8Array[]): WriteCandidate {
  return {
    kind: "none",
    resolve: async () => {
      const target = await verifiedTargetOfBinding(binding, personKeys);
      if (target === null) {
        noteRefusedBinding("none");
        return null;
      }
      return released("none", target, null, null);
    },
  };
}

/**
 * ADR-0060 section 6 - the mailbox binding of a person found in Everyone, as
 * a kept committer-delegation envelope with NO delegation names it: kept for
 * an add of the person `addedHexes` names (every key they have had), in the
 * group `scopeHex` (an envelope that recorded no group authorises nothing),
 * newest first, and passing the core's envelope check (the binding verifies
 * under the added account key). `null` when none does. The caller verifies
 * the binding again against the person's pinned keys before the write.
 */
export async function openWelcomeBinding(
  addedHexes: readonly string[],
  scopeHex: string,
): Promise<Uint8Array | null> {
  const session = reachSession();
  if (session === null) return null;
  const envelopes = delegationsForPerson(session.receivedGrants(), addedHexes).filter(
    (held) => held.scopeHex === scopeHex,
  );
  if (envelopes.length === 0) return null;
  const wasm = await loadWasm();
  for (const held of envelopes) {
    let env;
    try {
      env = wasm.decodeCommitterDelegationEnvelope(base64Decode(held.envelopeB64));
    } catch {
      continue;
    }
    try {
      if (env.delegationCount() !== 0) continue;
      if (!addedHexes.includes(journalHex(env.addedAccountKey))) continue;
      wasm.checkCommitterDelegations(env).forEach((d) => d.free());
      return env.addedMailboxBinding;
    } catch {
      /* a malformed envelope authorises nothing */
    } finally {
      env.free();
    }
  }
  return null;
}

/** Log the ADR-0078 continuity no-op. No identifier is logged. */
function noteContinuitySkipped(): void {
  // eslint-disable-next-line no-console
  console.info(
    "ADR-0078: a Welcome under the continuity key was skipped; no carrier names a reset account's new mailbox",
  );
}

/**
 * The continuity key a Welcome to `targetAccountKey` would have been signed
 * with (R.4), or `null`: only when that key is the one an accepted reset
 * re-pinned this device to, and only inside the key's window, counted from
 * the attestation's `signed_at` this device verified. Reads the pin store;
 * never throws. ADR-0078: used only to record the skipped write.
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
 * A write to a person's account mailbox: authorised with what is held for
 * the person (rule 2), each candidate's mailbox binding verified first
 * (ADR-0078), and re-checked once on a refusal (rule 6). `write` receives the
 * verified target to seal to and address.
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
    /** See {@link planWriteToPerson}. */
    readonly openBinding?: Uint8Array | null;
    /** See {@link planWriteToPerson}. */
    readonly exactKey?: boolean;
    /** ADR-0073 R.4: see {@link planWriteToPerson}. */
    readonly continuity?: ContinuityAuth | null;
    /** See {@link planWriteToPerson}. */
    readonly openWelcome?: boolean;
  },
  write: MailboxWrite<T>,
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
 *   - otherwise every kept grant for that account in order (contact, the
 *     scopes' `group`, their `channel`, then this group's `member`), then a
 *     committer delegation naming this account, moving to the next on `401`;
 *   - with nothing left, {@link MailboxUnreachableError} and no write.
 *
 * The web client cannot tell whether a person's mailbox is open (Everyone):
 * the DS answers an unsigned write to an open and to a closed mailbox alike.
 * So a Welcome for an account this device holds no grant for waits, owed,
 * until one arrives. One exception (ADR-0060 section 6, amended 2026-10-29):
 * when none of the above exists and a kept committer-delegation envelope with
 * NO delegation names the person for this group (their proposer found them
 * in Everyone; this device's own record when it proposed), the Welcome is
 * written unsigned to that envelope's verified binding, spending one
 * `mailbox-write` token, and counted delivered on a 2xx.
 *
 * ADR-0074 section 3.5a rule 6: with `recheck`, the first refusal re-checks
 * the person's address once. `write` receives the verified target.
 */
export async function writeWelcome<T>(
  targetAccountKey: Uint8Array,
  options: {
    readonly scope: Uint8Array;
    readonly explicit?: WasmReachGrant | string | null;
    readonly recheck?: AddressRecheck | null;
  },
  write: MailboxWrite<T>,
): Promise<T> {
  return writeToPerson(
    targetAccountKey,
    {
      scope: options.scope,
      explicit: options.explicit ?? null,
      allowMember: true,
      neverUnsigned: true,
      openWelcome: true,
      recheck: options.recheck ?? null,
    },
    write,
  );
}

/**
 * The delegation a kept committer-delegation envelope holds for one of this
 * account's keys, for an add of the person `addedHexes` names (every key
 * they have had), with the signer of the key it names and the added person's
 * mailbox binding the envelope carries (ADR-0078; verified by the caller
 * against the person's pinned keys before any write).
 *
 * ADR-0074 section 3.5a: a proposer names the committer's account key as IT
 * knows it. One that has observed this account's proven rotation names the
 * current key; one that has not names the key the role table carries, the
 * predecessor. So each own key this device holds the seed of is tried, the
 * current one first, and the write is signed with the key that was named.
 */
async function delegationFor(
  addedHexes: readonly string[],
): Promise<{ readonly header: string; readonly binding: Uint8Array; signer(): Promise<ProofSigner> } | null> {
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
      const binding = env.addedMailboxBinding;
      for (const delegate of delegates) {
        const header = wasm.committerDelegationFor(env, delegate.accountKey);
        if (header !== undefined) return { header, binding, signer: () => delegate.signer() };
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
