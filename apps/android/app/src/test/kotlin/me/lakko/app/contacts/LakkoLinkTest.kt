// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** The one reading of a Lakko link, shared by app links and scanned QR codes. */
class LakkoLinkTest {

    @Test
    fun `paths map to their kind with the fragment`() {
        assertEquals(LakkoLink.Contact("abcdEFGH", "sec"), lakkoLinkFromPath("/i/abcdEFGH", "sec"))
        assertEquals(LakkoLink.Group("abcdEFGH", null), lakkoLinkFromPath("/g/abcdEFGH/", " "))
        assertEquals(LakkoLink.Call("abcdEFGH", "s"), lakkoLinkFromPath("/c/abcdEFGH", "s"))
        assertNull(lakkoLinkFromPath("/i/", "sec"))
        assertNull(lakkoLinkFromPath("/feed/import", "x"))
    }

    @Test
    fun `a scanned string must be a Lakko URL`() {
        assertEquals(
            LakkoLink.Contact("abcdEFGH", "secret12"),
            lakkoLinkFromScanned("https://lakko.me/i/abcdEFGH#secret12"),
        )
        assertEquals(
            LakkoLink.Call("abcdEFGH", "x"),
            lakkoLinkFromScanned(" https://test.lakko.me/c/abcdEFGH#x "),
        )
        assertNull(lakkoLinkFromScanned("https://evil.example/i/abcdEFGH#x"))
        assertNull(lakkoLinkFromScanned("lakko://lakko.me/i/abcdEFGH#x"))
        assertNull(lakkoLinkFromScanned("https://lakko.me/settings"))
        assertNull(lakkoLinkFromScanned("not a url at all"))
        assertNull(lakkoLinkFromScanned("AAECAwQ"))
    }
}
