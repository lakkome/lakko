// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 R.2 / ADR-0050 - adding a person to a group checks WHOSE devices
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
 * Pure; `contacts/client.ts` supplies the binding reader.
 */
import type { AccountKeys } from "./predecessor";

/**
 * The account keys a device may be bound to and still be the expected
 * person's.
 *
 * | the expected key is in the account's pinned proven chain | accepted keys                         |
 * |-----------------------------------------------------------|---------------------------------------|
 * | no pin, or the pin's chain does not hold the expected key | the expected key alone                |
 * | yes (the current key or a proven predecessor)             | the current key and every predecessor |
 *
 * The second row is ADR-0066 section 3.5: after a PROVEN rotation a device
 * that was kept stays bound to the key it was enrolled under while a device
 * paired later binds to the new one, and both are the same person. A key an
 * accepted RESET retired is in no proven chain, so it is never accepted
 * through this row.
 */
export function acceptableBoundKeys(
  expectedKeyB64: string,
  accountKeys: AccountKeys | undefined,
): ReadonlySet<string> {
  const out = new Set<string>();
  if (expectedKeyB64.length === 0) return out;
  out.add(expectedKeyB64);
  if (accountKeys === undefined) return out;
  const chain = [accountKeys.currentKeyB64, ...accountKeys.provenPredecessorsB64];
  if (!chain.includes(expectedKeyB64)) return out;
  for (const k of chain) if (k.length > 0) out.add(k);
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
