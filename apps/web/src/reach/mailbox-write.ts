// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0058 section 6 - choosing the key a write to someone's account mailbox
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
import { bestGrantFor, delegationsFor, type HeldGrant } from "./received-grants";
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
