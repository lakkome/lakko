// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import org.junit.Assert.assertEquals
import org.junit.Test

/** The 2026-09-28 security pass — a link token carries its secret as the
 *  fragment, `<slug>#<secret>`, through the router. */
class LinkTokenTest {

    @Test
    fun `a token splits into slug and fragment`() {
        assertEquals("AbCdEfGh" to "SECRETSECRETSECR", splitLinkToken("AbCdEfGh#SECRETSECRETSECR"))
        assertEquals("AbCdEfGh" to null, splitLinkToken("AbCdEfGh"))
        assertEquals("AbCdEfGh" to null, splitLinkToken("AbCdEfGh/#"))
    }

    @Test
    fun `join is the inverse of split`() {
        assertEquals("s#f", joinLinkToken("s", "f"))
        assertEquals("s", joinLinkToken("s", null))
        assertEquals("s", joinLinkToken("s", " "))
        val token = "slug#frag"
        val (slug, fragment) = splitLinkToken(token)
        assertEquals(token, joinLinkToken(slug, fragment))
    }
}
