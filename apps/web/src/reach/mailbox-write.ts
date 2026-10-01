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
import { base64Decode } from "../auth/account-id";
import { loadWasm } from "../wasm";
import { bestGrantFor, delegationsFor, grantsInOrder, type HeldGrant } from "./received-grants";
import { reachSession, reachSignerForGrant, reachSignerForGrantWire } from "./reach-service";

/** Which rule authorised a write. */
export type WriteAuthKind = "own" | "explicit" | "kept" | "delegation" | "none";

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
  const targetHex = journalHex(targetAccountKey);
  const isOwnAccount = session !== null && session.accountKeyHex === targetHex;
  const kept =
    session === null
      ? null
      : bestGrantFor(session.receivedGrants(), targetHex, {
          ...(options.scope === undefined ? {} : { scopeHex: journalHex(options.scope) }),
          allowMember: options.allowMember === true,
        });
  const delegationHeader =
    session === null ? null : await delegationHeaderFor(targetHex, session.accountKey);
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
      return released(kind, await session!.accountReachSigner(), delegationHeader);
    case "none":
      return released(kind, null, null);
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

/**
 * A Welcome write to `targetAccountKey`'s account mailbox that is never sent
 * unsigned (ADR-0050 section 2.5a item 7):
 *
 *   - this account's own mailbox and an explicit grant: exactly as
 *     {@link resolveWriteAuth} does them, one attempt;
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
 */
export async function writeWelcome<T>(
  targetAccountKey: Uint8Array,
  options: {
    readonly scope: Uint8Array;
    readonly explicit?: WasmReachGrant | string | null;
  },
  write: (auth: MailboxWriteAuth, kind: WriteAuthKind) => Promise<T>,
): Promise<T> {
  const session = reachSession();
  const targetHex = journalHex(targetAccountKey);
  const isOwnAccount = session !== null && session.accountKeyHex === targetHex;
  const hasExplicit = options.explicit !== undefined && options.explicit !== null;
  if (isOwnAccount || hasExplicit) {
    const resolved = await resolveWriteAuth(targetAccountKey, {
      scope: options.scope,
      explicit: options.explicit ?? null,
      allowMember: true,
    });
    try {
      return await write(resolved.auth, resolved.kind);
    } finally {
      resolved.release();
    }
  }
  const candidates: WriteCandidate[] = [];
  if (session !== null) {
    for (const held of grantsInOrder(session.receivedGrants(), targetHex, {
      scopeHex: journalHex(options.scope),
      allowMember: true,
    })) {
      candidates.push({
        kind: "kept",
        resolve: async () => released("kept", await reachSignerForGrantWire(held.wireB64), null),
      });
    }
    const delegationHeader = await delegationHeaderFor(targetHex, session.accountKey);
    if (delegationHeader !== null) {
      candidates.push({
        kind: "delegation",
        resolve: async () => released("delegation", await session.accountReachSigner(), delegationHeader),
      });
    }
  }
  return writeThroughCandidates(candidates, write);
}

/** The delegation header a kept committer-delegation envelope holds for
 *  our account key, newest envelope first; `null` when none does. */
async function delegationHeaderFor(
  addedHex: string,
  ownAccountKey: Uint8Array,
): Promise<string | null> {
  const session = reachSession();
  if (session === null) return null;
  const envelopes = delegationsFor(session.receivedGrants(), addedHex);
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
      const header = wasm.committerDelegationFor(env, ownAccountKey);
      if (header !== undefined) return header;
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
