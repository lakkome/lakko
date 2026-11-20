// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import org.junit.Test
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue

class DsErrorRedactionTest {
    @Test
    fun anErrorMessageNamesNoJournal() {
        val id = "ab".repeat(32)
        val err = DsHttpError(status = 401, endpoint = "/ds/group/$id/messages?after=17", body = "{}")
        assertEquals("ds /ds/group/:id/messages → 401: {}", err.message)
        assertFalse(err.message!!.contains(id))
        assertTrue(err.endpoint.contains(id))
        assertEquals("/ds/mailbox/:id", redactedEndpoint("/ds/mailbox/0123456789abcdef0123"))
        val net = DsNetworkError("/ds/group/$id/messages", RuntimeException("timeout"))
        assertFalse(net.message!!.contains(id))
        val refused = DsProofRefusedError("/ds/group/$id/messages", clockSkewed = false, body = "{}")
        assertEquals("ds /ds/group/:id/messages → 401: proof refused", refused.message)
    }
}
