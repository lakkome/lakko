// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * A call invitation sent as a message (`docs/ux/call-links.md` section 3).
 * The test list is the web client's `call-invite-link.test.ts`: the three
 * clients must agree on which message is an invitation and which is text.
 */
class CallInviteMessageTest {
    private val origin = "https://lakko.example"

    @Test
    fun `a message that is one call link of this deployment is an invitation`() {
        assertEquals(
            CallInviteTarget(slug = "AbC_123-x", secret = "secretSECRET12"),
            callInviteOf("$origin/c/AbC_123-x#secretSECRET12", origin),
        )
        assertEquals("abc#def", callInviteOf("  $origin/c/abc#def \n", origin)?.argument)
    }

    @Test
    fun `the join target is what the app's own link handling takes`() {
        // The same one-string form a tapped or a pasted `/c/` link arrives as.
        val target = callInviteOf("$origin/c/abc#def", origin)
        assertEquals(callInviteSlugFromInput("$origin/c/abc#def"), target?.argument)
    }

    @Test
    fun `a link to another host stays plain text`() {
        assertNull(callInviteOf("https://lakko.example.evil.test/c/abc#def", origin))
        assertNull(callInviteOf("http://lakko.example/c/abc#def", origin))
        assertNull(callInviteOf("https://user@lakko.example/c/abc#def", origin))
        assertNull(callInviteOf("https://lakko.example:8443/c/abc#def", origin))
    }

    @Test
    fun `ordinary text, and text around a link, is left alone`() {
        assertNull(callInviteOf("", origin))
        assertNull(callInviteOf("call me", origin))
        assertNull(callInviteOf("join $origin/c/abc#def", origin))
        assertNull(callInviteOf("$origin/c/abc#def now", origin))
        assertNull(callInviteOf("$origin/c/abc#def\n$origin/c/abc#def", origin))
    }

    @Test
    fun `the call path and the secret after the hash are required`() {
        assertNull(callInviteOf("$origin/c/abc", origin))
        assertNull(callInviteOf("$origin/g/abc#def", origin))
        assertNull(callInviteOf("$origin/c/abc/extra#def", origin))
        assertNull(callInviteOf("$origin/c/abc?x=1#def", origin))
        assertNull(callInviteOf("$origin/c/a%20b#def", origin))
        assertNull(callInviteOf("$origin/c/#def", origin))
        assertNull(callInviteOf("$origin/c/abc#", origin))
    }

    @Test
    fun `the host and the scheme are compared as an origin, not as text`() {
        assertEquals("abc#def", callInviteOf("HTTPS://LAKKO.EXAMPLE/c/abc#def", origin)?.argument)
        assertEquals("abc#def", callInviteOf("https://lakko.example:443/c/abc#def", origin)?.argument)
        // The origin this build mints under may carry a trailing slash.
        assertEquals("abc#def", callInviteOf("$origin/c/abc#def", "$origin/")?.argument)
    }

    @Test
    fun `something that is not a link at all is text`() {
        assertNull(callInviteOf("lakko.example/c/abc#def", origin))
        assertNull(callInviteOf("mailto:someone@lakko.example", origin))
        assertNull(callInviteOf("https://", origin))
        assertNull(callInviteOf("$origin/c/abc#def", "not an origin"))
    }
}
