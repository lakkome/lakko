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

/*
 * ADR-0066 section 3.5 x ADR-0065 R.2 - the ONE filter every path that adds
 * a person's devices to a group goes through: the user-facing add, a new
 * conversation, a successor build, the re-admission arm and the ADR-0050
 * membership re-request.
 *
 * After a PROVEN rotation an account's devices stay bound to the key they
 * were enrolled under (a predecessor) while the directory serves the new
 * one. So the add first runs the identity gate for the account, which
 * records the proven step (and turns the verified binding of a non-contact
 * into a pin with its chain), and only then decides which bindings are the
 * person's.
 */

/**
 * What the add tells the identity gate it holds for the account.
 *
 * | this device holds a pin or a verified binding | told to the gate                       |
 * |-----------------------------------------------|----------------------------------------|
 * | yes                                           | nothing: the gate judges from its own  |
 * | no                                            | the expected key, as a roster key      |
 *
 * Row 1: a changed key that comes with a valid proof is re-pinned with the
 * old key recorded as a proven predecessor; a verified binding becomes the
 * pin. Row 2 never pins: the gate records a verified binding when the
 * directory serves exactly the expected key and establishes nothing
 * otherwise ([memberHeldKey]), so an add can never blind-TOFU an account.
 */
public fun gateHeldKeyForAdd(pinned: Boolean, hasBinding: Boolean, expectedKeyB64: String?): String? =
    if (pinned || hasBinding) null else expectedKeyB64?.takeIf { it.isNotEmpty() }

/**
 * Whether the add runs the identity gate at all.
 *
 * | holds a pin or a binding | a person key is named | the gate runs |
 * |--------------------------|-----------------------|---------------|
 * | yes                      | any                   | yes           |
 * | no                       | yes                   | yes (row 2 of [gateHeldKeyForAdd]) |
 * | no                       | no                    | no            |
 *
 * The last row is a re-request for an account this device trusts no key
 * for: the gate, told nothing, would pin whatever the directory serves on
 * first use. Nobody is added under such an id ([addFilterKeys]), so the
 * directory is not asked either.
 */
public fun addRunsGate(pinned: Boolean, hasBinding: Boolean, expectedKeyB64: String?): Boolean =
    pinned || hasBinding || !expectedKeyB64.isNullOrEmpty()

/**
 * The account keys a device may be bound to and be added as the expected
 * person's, read AFTER the identity gate ran.
 *
 * | expected key                     | this device holds                 | accepted keys                                   |
 * |----------------------------------|-----------------------------------|-------------------------------------------------|
 * | one an accepted reset retired    | anything                          | none                                            |
 * | in the pinned proven chain       | a pin                             | the pinned key and every proven predecessor     |
 * | not in the chain, or no pin      | anything                          | the expected key alone                          |
 * | not named (a re-request)         | a pin                             | the pinned key and every proven predecessor     |
 * | not named                        | a verified binding only           | the binding's key                               |
 * | not named                        | nothing                           | none                                            |
 *
 * A key an accepted RESET retired is removed from every row: it is in no
 * proven chain, and a device still bound to it belongs to nobody this
 * device trusts.
 *
 * [pinnedChainB64] is the pinned key with its proven predecessors, or `null`
 * when the account is not pinned; [bindingKeyB64] the verified binding's key
 * of an account that is not pinned.
 */
public fun addFilterKeys(
    expectedKeyB64: String?,
    pinnedChainB64: List<String>?,
    bindingKeyB64: String?,
    retiredKeysB64: Set<String>,
): Set<String> {
    val expected = expectedKeyB64?.takeIf { it.isNotEmpty() }
        ?: pinnedChainB64?.lastOrNull { it.isNotEmpty() }
        ?: bindingKeyB64?.takeIf { it.isNotEmpty() }
        ?: return emptySet()
    if (expected in retiredKeysB64) return emptySet()
    return acceptableBoundKeys(expected, pinnedChainB64).filterNotTo(LinkedHashSet()) { it in retiredKeysB64 }
}
