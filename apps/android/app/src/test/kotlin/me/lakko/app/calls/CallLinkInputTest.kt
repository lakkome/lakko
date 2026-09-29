// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * docs/ux/navigation.md section 5.2 — "Join by link".
 *
 * What arrives is whatever the clipboard held, so every shape a sharing app
 * produces is a case here rather than a thing to try on a device.
 */
class CallLinkInputTest {

    @Test
    fun `a full link yields its slug`() {
        assertEquals("abc123", callInviteSlugFromInput("https://lakko.me/c/abc123"))
    }

    @Test
    fun `whitespace and a trailing slash are not part of the slug`() {
        assertEquals("abc123", callInviteSlugFromInput("  https://lakko.me/c/abc123/  "))
    }

    @Test
    fun `a query a sharing app appended is dropped`() {
        assertEquals("abc123", callInviteSlugFromInput("https://lakko.me/c/abc123?utm=mail"))
    }

    @Test
    fun `the fragment is the link secret and is kept with the slug`() {
        // The 2026-09-28 security pass: the capability is sealed under the
        // secret the fragment carries, so dropping it would make the link
        // unopenable.
        assertEquals("abc123#SECRET", callInviteSlugFromInput("https://lakko.me/c/abc123#SECRET"))
        assertEquals("abc123#SECRET", callInviteSlugFromInput("https://lakko.me/c/abc123/?utm=mail#SECRET"))
        assertEquals("abc123#SECRET", callInviteSlugFromInput("abc123#SECRET"))
        assertEquals("abc123", callInviteSlugFromInput("https://lakko.me/c/abc123#"))
    }

    @Test
    fun `a bare slug is taken as itself`() {
        // Somebody who copied only the tail of a link meant the link.
        assertEquals("abc123", callInviteSlugFromInput("abc123"))
    }

    @Test
    fun `another kind of link is never guessed at as a call`() {
        // `/i/` is a contact invite and `/g/` a group ticket. Silently joining
        // a call from either would act on something nobody asked for.
        assertNull(callInviteSlugFromInput("https://lakko.me/i/abc123"))
        assertNull(callInviteSlugFromInput("https://lakko.me/g/abc123"))
        assertNull(callInviteSlugFromInput("https://example.com/abc123"))
    }

    @Test
    fun `nothing typed is nothing to join`() {
        assertNull(callInviteSlugFromInput(""))
        assertNull(callInviteSlugFromInput("   "))
        assertNull(callInviteSlugFromInput("https://lakko.me/c/"))
        assertNull(callInviteSlugFromInput("/"))
    }
}
