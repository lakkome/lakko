// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0042, "A wake is shown only for the signed-in account" (amended
 * 2026-09-30), with ADR-0078 section 3 — the pure gate [wakeAccountFor].
 *
 * A wake is handled only when a binding exists and the wake carries a
 * well-formed `s` (a 16-byte subscription id); it is then handled as the
 * bound account. Whether the account holds that id for the wake's `g` is the
 * route store's check (`PushAccountScopeStoresTest`).
 */
class PushWakeGateTest {

    private val sub = "0a".repeat(16)
    private val account = "acct-A=="
    private val binding = PushWakeBinding(accountIdB64 = account)

    @Test
    fun `no binding refuses every wake`() {
        assertNull(wakeAccountFor(sub, null))
        assertNull(wakeAccountFor(null, null))
    }

    @Test
    fun `a wake without s or with a malformed s is refused`() {
        assertNull(wakeAccountFor(null, binding))
        assertNull(wakeAccountFor("", binding))
        assertNull(wakeAccountFor("zz".repeat(16), binding))
        assertNull(wakeAccountFor(sub.dropLast(1), binding))
        // A 32-byte device pseudonym (the retired `d`) is not a subscription id.
        assertNull(wakeAccountFor("0a".repeat(32), binding))
    }

    @Test
    fun `a well-formed s is handled as the bound account`() {
        assertEquals(account, wakeAccountFor(sub, binding))
        assertEquals(account, wakeAccountFor(" " + sub.uppercase() + "\n", binding))
    }

    @Test
    fun `a binding with no account refuses`() {
        assertNull(wakeAccountFor(sub, binding.copy(accountIdB64 = "")))
    }
}
