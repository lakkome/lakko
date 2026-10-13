// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 / ADR-0050 - adding a person to a group checks WHOSE devices
 * the directory served.
 *
 * `ContactsClient.addMemberToGroup` is told an account id (what the
 * directory is asked for) and the account key the caller expects that
 * person to have. The directory is not trusted to say whose devices it
 * returned: every KeyPackage carries a signed device binding naming an
 * account key (ADR-0050), and a device whose binding names another key than
 * the expected one is not that person's device. It is never added, whoever
 * the caller is: the rule lives in the function that adds, so no call site
 * can forget it.
 *
 * Every path that adds an account's devices takes the accepted keys from
 * this module: the user-facing add, a successor build and a new chat
 * (`acceptableBoundKeys` with the expected key), and the ADR-0050
 * re-admission arm and membership re-request (`trustedBoundKeys`, which has
 * no caller's key and reads what this device trusts for the account id).
 *
 * Pure; `contacts/client.ts` supplies the binding reader.
 */
import type { AccountKeys } from "./predecessor";

/** A set of retired keys in any of the shapes the stores hand out. */
type KeySet = ReadonlySet<string> | ReadonlyMap<string, unknown>;

const NO_KEYS: ReadonlySet<string> = new Set();

/**
 * The account keys a device may be bound to and still be the expected
 * person's. Call it AFTER the identity gate ran for the account, so a proven
 * rotation is already recorded in `accountKeys`.
 *
 * | the expected key is ...                                    | accepted keys                         |
 * |------------------------------------------------------------|---------------------------------------|
 * | a key an accepted reset retired                            | none                                  |
 * | not in the account's pinned proven chain (or no pin)       | the expected key alone                |
 * | in the chain (the current key or a proven predecessor)     | the current key and every predecessor |
 *
 * The third row is ADR-0074 section 3.5: after a PROVEN rotation a device
 * that was kept stays bound to the key it was enrolled under while a device
 * paired later may bind to the new one, and both are the same person. A key
 * an accepted RESET retired (`supersededKeysB64`) is never accepted, whoever
 * asks for it and whatever chain names it: old keys do not come back.
 */
export function acceptableBoundKeys(
  expectedKeyB64: string,
  accountKeys: AccountKeys | undefined,
  supersededKeysB64: KeySet = NO_KEYS,
): ReadonlySet<string> {
  const out = new Set<string>();
  if (expectedKeyB64.length === 0 || supersededKeysB64.has(expectedKeyB64)) return out;
  out.add(expectedKeyB64);
  if (accountKeys === undefined) return out;
  const chain = [accountKeys.currentKeyB64, ...accountKeys.provenPredecessorsB64];
  if (!chain.includes(expectedKeyB64)) return out;
  for (const k of chain) if (k.length > 0 && !supersededKeysB64.has(k)) out.add(k);
  return out;
}

/**
 * The account keys this device TRUSTS for an account id, for a path that has
 * no caller's key (the re-admission arm, the membership re-request): the
 * pin's current key and proven predecessors, else the key of the binding
 * this device verified against the directory. Never a key an accepted reset
 * retired. An account that is neither pinned nor bound has no trusted key:
 * nobody is added under its id.
 */
export function trustedBoundKeys(
  accountKeys: AccountKeys | undefined,
  verifiedBindingKeyB64: string | undefined,
  supersededKeysB64: KeySet = NO_KEYS,
): ReadonlySet<string> {
  const out = new Set<string>();
  const candidates =
    accountKeys !== undefined
      ? [accountKeys.currentKeyB64, ...accountKeys.provenPredecessorsB64]
      : verifiedBindingKeyB64 !== undefined
        ? [verifiedBindingKeyB64]
        : [];
  for (const k of candidates) if (k.length > 0 && !supersededKeysB64.has(k)) out.add(k);
  return out;
}

/**
 * The devices that are the expected person's: those whose binding names an
 * accepted key. `boundKeyOf` returns the account key a KeyPackage's binding
 * names (the leaf key for an unbound Phase 1 leaf), or `null` for a binding
 * that does not verify or a KeyPackage that does not parse; both are
 * refused. Order is kept.
 */
export function devicesBoundTo<D>(
  devices: readonly D[],
  acceptedKeysB64: ReadonlySet<string>,
  boundKeyOf: (device: D) => string | null,
): D[] {
  return devices.filter((d) => {
    const bound = boundKeyOf(d);
    return bound !== null && acceptedKeysB64.has(bound);
  });
}
