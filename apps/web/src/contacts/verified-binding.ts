// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 - the VERIFIED BINDING of an account this device never
 * pinned, and the one rule that decides what a group member's roster key is
 * worth against the directory.
 *
 * The acceptance rule (`reset-verdict.ts`) judges a change from "the key
 * this device HOLDS for the account". For a contact that is the pin. For a
 * group member this device never pinned, R.2 names "the roster's account
 * key for that account id", and the account id of such a member comes from
 * the member's OWN profile envelope. A member can write any account id
 * there. If the roster key were taken as the held key on that claim alone,
 * a member could name ANOTHER account's id; once that account ever reset,
 * an admin's client would read "member key -> that account's key" as a
 * reset, mark the member superseded, propose its removal and add the other
 * account's devices in its place.
 *
 * So a profile's claimed id establishes nothing. What does is a binding
 * this device saw for itself:
 *
 *   `(account id -> account key)` is recorded ONLY when this device itself
 *   fetched that account id's bundle through the fail-closed gate and the
 *   fetched key EQUALLED the roster key.
 *
 * The reset path for a non-pinned member runs only from such a binding, and
 * only when it names exactly the member's roster key. The first observation
 * is never a reset: with no binding, a directory that serves the roster key
 * records one now; a directory that serves another key yields no verdict,
 * and nothing is done and nothing is shown.
 *
 * A pinned account never reaches this module: the pin is the verified
 * binding. An accepted reset and a proven rotation both write a pin, after
 * which the binding is dropped.
 *
 * The rule is pure; the store below is `localStorage` per signed-in
 * account, exactly as the pin store is (`local-store.ts`, not sealed under
 * the device key: it holds public keys and account ids). Never carried to
 * another device: a binding is what THIS device fetched. Removed with the
 * account's data (`auth/wipe-account.ts`). iOS and Android mirror the rule
 * table exactly (R.12).
 */
import { boundContactsAccount } from "./local-store";
import type { AccountKeys } from "./predecessor";

/** One verified binding. */
export interface VerifiedBinding {
  /** The account key the directory served for the account id, standard
   *  base64, equal to the roster key at the moment it was recorded. */
  readonly keyB64: string;
  /** The bundle's `key_generation` at that moment: the accepted-generation
   *  baseline a later reset must be strictly above (R.2 rule 2). */
  readonly keyGeneration: number;
  /** When this device recorded it, UNIX milliseconds. */
  readonly verifiedAtMs: number;
  /** ADR-0074 section 3.7: the key the account's conversations are bound to,
   *  from a two-way conversation link that verified under `keyB64`: a proven
   *  predecessor of `keyB64`, for an account this device has no pin to hold
   *  one on. */
  readonly conversationKeyB64?: string;
}

/** What a group member's roster key is worth for one account id. */
export type MemberHeldVerdict =
  /** The account is pinned: the pin decides and the roster key is ignored. */
  | { readonly kind: "pin" }
  /** No binding, and the directory serves exactly the roster key: record the
   *  binding now. A first observation, never a reset. */
  | { readonly kind: "record" }
  /** The binding names the roster key and the directory still serves it. */
  | { readonly kind: "confirmed" }
  /** The binding names the roster key and the directory serves ANOTHER key:
   *  a change from a key this device verified. It is judged from the binding
   *  exactly as from a pin (a proof: ADR-0016; none: the reset rule). */
  | { readonly kind: "held"; readonly heldKeyB64: string; readonly acceptedGeneration: number }
  /** Nothing is established for this roster key under this account id: no
   *  verdict, nothing done, nothing shown. */
  | { readonly kind: "none" };

/**
 * | pinned | binding | binding names the roster key | directory serves the roster key | verdict     |
 * |--------|---------|------------------------------|---------------------------------|-------------|
 * | yes    | -       | -                            | -                               | `pin`       |
 * | no     | none    | -                            | yes                             | `record`    |
 * | no     | none    | -                            | no                              | `none`      |
 * | no     | held    | yes                          | yes                             | `confirmed` |
 * | no     | held    | yes                          | no                              | `held`      |
 * | no     | held    | no                           | any                             | `none`      |
 *
 * Row 3 is the attack this module exists for (a member claiming another
 * account's id, that account having reset at any time) and also the honest
 * first sight of a member whose account already moved on: neither is told
 * apart here, and neither is acted on. Row 6 is the same claim made after
 * this device already verified the account under its real key.
 *
 * One addition to row 6 (ADR-0074 section 3.7): a roster key that is the
 * binding's conversation key, proven by a link signed by both keys, while
 * the directory still serves the binding's key, is `confirmed`.
 */
export function memberHeldKey(input: {
  readonly pinned: boolean;
  readonly binding: VerifiedBinding | undefined;
  /** The PERSON key the roster names for the member, standard base64. */
  readonly rosterKeyB64: string;
  /** The key the directory serves for the account id right now. */
  readonly fetchedKeyB64: string;
}): MemberHeldVerdict {
  if (input.pinned) return { kind: "pin" };
  const binding = input.binding;
  if (binding === undefined) {
    return input.fetchedKeyB64 === input.rosterKeyB64 ? { kind: "record" } : { kind: "none" };
  }
  if (binding.keyB64 !== input.rosterKeyB64) {
    return binding.conversationKeyB64 === input.rosterKeyB64 && input.fetchedKeyB64 === binding.keyB64
      ? { kind: "confirmed" }
      : { kind: "none" };
  }
  if (input.fetchedKeyB64 === input.rosterKeyB64) return { kind: "confirmed" };
  return {
    kind: "held",
    heldKeyB64: binding.keyB64,
    acceptedGeneration: binding.keyGeneration,
  };
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/** The row's key; the signed-in account id (standard base64) follows. */
export const VERIFIED_BINDINGS_PREFIX = "lakko.contacts.verified-bindings.v1.";

function ownerOf(owner: string | undefined): string | null {
  if (owner !== undefined) return owner.length === 0 ? null : owner;
  return boundContactsAccount();
}

function isBinding(v: unknown): v is VerifiedBinding {
  if (typeof v !== "object" || v === null) return false;
  const b = v as Partial<VerifiedBinding>;
  return (
    typeof b.keyB64 === "string" &&
    b.keyB64.length > 0 &&
    typeof b.keyGeneration === "number" &&
    Number.isSafeInteger(b.keyGeneration) &&
    b.keyGeneration >= 0 &&
    typeof b.verifiedAtMs === "number" &&
    Number.isFinite(b.verifiedAtMs) &&
    (b.conversationKeyB64 === undefined ||
      (typeof b.conversationKeyB64 === "string" && b.conversationKeyB64.length > 0))
  );
}

function load(owner: string | null): Record<string, VerifiedBinding> {
  if (owner === null || typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(VERIFIED_BINDINGS_PREFIX + owner);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const out: Record<string, VerifiedBinding> = {};
    // A junk entry is dropped, which reads as "no binding": the fail-closed
    // direction (nothing is judged from it).
    for (const [accountIdB64, value] of Object.entries(parsed)) {
      if (isBinding(value)) out[accountIdB64] = value;
    }
    return out;
  } catch {
    return {};
  }
}

function save(owner: string, all: Record<string, VerifiedBinding>): void {
  try {
    localStorage.setItem(VERIFIED_BINDINGS_PREFIX + owner, JSON.stringify(all));
  } catch {
    /* storage unavailable: no binding is kept, so nothing is judged */
  }
}

/** The verified binding of `accountIdB64`, or `undefined`. */
export function readVerifiedBinding(accountIdB64: string, owner?: string): VerifiedBinding | undefined {
  return load(ownerOf(owner))[accountIdB64];
}

/**
 * Record a binding. WRITE-ONCE: a held binding is never replaced here (a
 * change from it is a rotation or a reset, which ends in a pin, never in a
 * second binding). Returns whether it was written.
 */
export function recordVerifiedBinding(
  accountIdB64: string,
  binding: VerifiedBinding,
  owner?: string,
): boolean {
  const account = ownerOf(owner);
  if (account === null || !isBinding(binding)) return false;
  const all = load(account);
  if (all[accountIdB64] !== undefined) return false;
  all[accountIdB64] = binding;
  save(account, all);
  return true;
}

/**
 * ADR-0074 section 3.7: record the conversation key a link that verified
 * under `keyB64` named. Written only on a binding of exactly that key, and
 * the one change a held binding takes: it adds what the key signed, and the
 * key itself stays. Returns whether anything changed.
 */
export function recordBindingConversationKey(
  accountIdB64: string,
  keyB64: string,
  conversationKeyB64: string,
  owner?: string,
): boolean {
  const account = ownerOf(owner);
  if (account === null || conversationKeyB64.length === 0 || conversationKeyB64 === keyB64) return false;
  const all = load(account);
  const held = all[accountIdB64];
  if (held === undefined || held.keyB64 !== keyB64) return false;
  if (held.conversationKeyB64 === conversationKeyB64) return false;
  all[accountIdB64] = { ...held, conversationKeyB64 };
  save(account, all);
  return true;
}

/** A binding as `predecessor.ts` reads an account's keys. */
export function accountKeysOfBinding(binding: VerifiedBinding | undefined): AccountKeys | undefined {
  if (binding === undefined) return undefined;
  return {
    currentKeyB64: binding.keyB64,
    provenPredecessorsB64: binding.conversationKeyB64 === undefined ? [] : [binding.conversationKeyB64],
  };
}

/** The keys of every account this device holds a proven link for through a
 *  binding: the predecessor table's input beside the pins. A binding with no
 *  conversation key adds nothing to that table. */
export function loadBoundAccountKeys(owner?: string): AccountKeys[] {
  const out: AccountKeys[] = [];
  for (const binding of Object.values(load(ownerOf(owner)))) {
    if (binding.conversationKeyB64 === undefined) continue;
    const keys = accountKeysOfBinding(binding);
    if (keys !== undefined) out.push(keys);
  }
  return out;
}

/** Drop a binding (the account is pinned now: the pin is the binding). */
export function dropVerifiedBinding(accountIdB64: string, owner?: string): void {
  const account = ownerOf(owner);
  if (account === null) return;
  const all = load(account);
  if (all[accountIdB64] === undefined) return;
  delete all[accountIdB64];
  save(account, all);
}

/** Forget every binding of the account (its data is wiped). */
export function clearVerifiedBindingsForAccount(accountIdB64: string): void {
  try {
    localStorage.removeItem(VERIFIED_BINDINGS_PREFIX + accountIdB64);
  } catch {
    /* nothing to clear */
  }
}
