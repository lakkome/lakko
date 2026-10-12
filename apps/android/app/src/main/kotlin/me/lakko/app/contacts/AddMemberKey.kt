// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

/*
 * ADR-0065 R.2 / ADR-0050 - adding a person to a group checks WHOSE devices
 * the directory served.
 *
 * The function that adds is told an account id (what the directory is asked
 * for) and the account key the caller expects that person to have. The
 * directory is not trusted to say whose devices it returned: every KeyPackage
 * carries a signed device binding naming an account key (ADR-0050), and a
 * device whose binding names another key than the expected one is not that
 * person's device. It is never added, whoever the caller is: the rule lives
 * in the function that adds, so no call site can forget it. Mirror of
 * `apps/web/src/contacts/add-member-key.ts`.
 */

/**
 * The account keys a device may be bound to and still be the expected
 * person's.
 *
 * | the expected key is in the account's pinned proven chain  | accepted keys                         |
 * |-----------------------------------------------------------|---------------------------------------|
 * | no pin, or the pin's chain does not hold the expected key | the expected key alone                |
 * | yes (the current key or a proven predecessor)             | the current key and every predecessor |
 *
 * A key an accepted RESET retired is in no proven chain, so it is never
 * accepted through the second row.
 *
 * [pinnedChainB64] is the pinned key and its proven predecessors, or `null`
 * when the account is not pinned.
 */
public fun acceptableBoundKeys(expectedKeyB64: String, pinnedChainB64: List<String>?): Set<String> {
    val out = LinkedHashSet<String>()
    if (expectedKeyB64.isEmpty()) return out
    out += expectedKeyB64
    if (pinnedChainB64 == null || expectedKeyB64 !in pinnedChainB64) return out
    for (k in pinnedChainB64) if (k.isNotEmpty()) out += k
    return out
}

/**
 * The devices that are the expected person's: those whose binding names an
 * accepted key. [boundKeyOf] returns the account key a KeyPackage's binding
 * names (the leaf key for an unbound Phase 1 leaf), or `null` for a binding
 * that does not verify or a KeyPackage that does not parse; both are refused.
 * Order is kept.
 */
public fun <D> devicesBoundTo(devices: List<D>, acceptedKeysB64: Set<String>, boundKeyOf: (D) -> String?): List<D> =
    devices.filter { d -> boundKeyOf(d)?.let { it in acceptedKeysB64 } == true }
