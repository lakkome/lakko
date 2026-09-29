// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Test

/** `docs/ux/anti-abuse.md` section 2 — `handle-bind-limited`. */
class HandleBindLimitTest {

    private fun limited(retryAfter: Long?) = AuthHttpError(
        status = 429,
        endpoint = "/auth/login-handle",
        body = """{"error":"handle-bind-limited"}""",
        code = "handle-bind-limited",
        retryAfterSecs = retryAfter,
    )

    @Test
    fun `the wait rounds up to whole minutes`() {
        assertEquals(
            "You have changed your username too often. Try again in 3 minutes.",
            authLimitCopy(limited(121)),
        )
        assertEquals(
            "You have changed your username too often. Try again in 60 minutes.",
            authLimitCopy(limited(3600)),
        )
    }

    @Test
    fun `over an hour or with no stated wait it says later`() {
        assertEquals("You have changed your username too often. Try again later.", authLimitCopy(limited(3601)))
        assertEquals("You have changed your username too often. Try again later.", authLimitCopy(limited(null)))
    }
}
