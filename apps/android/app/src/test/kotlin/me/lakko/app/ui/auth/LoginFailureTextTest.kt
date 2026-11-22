// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.PasswordLoginRejected
import me.lakko.app.auth.passwordLoginRejectedOr
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException

/** Only a refused password reads "Wrong username or password" (2026-11-22). */
class LoginFailureTextTest {

    private val wrong = UiText("auth.error.wrong_credentials")
    private val generic = UiText("common.error.unavailable")

    @Test
    fun theCoresOpaqueFailureAndNothingElseIsARefusedPassword() {
        assertTrue(passwordLoginRejectedOr(FfiException.Auth("auth error: OPAQUE protocol failure")) is PasswordLoginRejected)
        val other = FfiException.Auth("auth error: invalid OPAQUE wire payload")
        assertSame(other, passwordLoginRejectedOr(other))
        val finalized = FfiException.AuthAlreadyFinalized("x")
        assertSame(finalized, passwordLoginRejectedOr(finalized))
    }

    @Test
    fun onlyARefusedPasswordShowsTheWrongCredentialsSentence() {
        assertEquals(wrong, loginFailureText(PasswordLoginRejected()))
        assertEquals(wrong, loginFailureText(AuthHttpError(401, "/login/finish", "")))
        assertEquals(wrong, loginFailureText(AuthClientArgError("password must be non-empty")))
        assertEquals(wrong, loginFailureText(AuthClientArgError("username must be non-empty")))

        assertEquals(generic, loginFailureText(FfiException.Auth("auth error: invalid OPAQUE wire payload")))
        assertEquals(generic, loginFailureText(AuthHttpError(404, "/login/start", "")))
        assertEquals(generic, loginFailureText(AuthHttpError(500, "/login/finish", "")))
        assertEquals(generic, loginFailureText(AuthHttpError(200, "/login/start", "decode failed")))
        assertEquals(generic, loginFailureText(AuthClientArgError("login handle must be 32 bytes")))
        assertEquals(generic, loginFailureText(IllegalStateException("unnamed")))

        assertEquals(UiText("common.error.offline"), loginFailureText(AuthNetworkError("/login/start", java.io.IOException("down"))))
    }

    @Test
    fun theLogLineNamesTheKindAndStatusButNoMessageOrBody() {
        assertEquals(
            "error=AuthHttpError status=401 endpoint=/login/finish",
            loginFailureLogLine(AuthHttpError(401, "/login/finish", "secret body alice")),
        )
        assertEquals("error=Auth status=none", loginFailureLogLine(FfiException.Auth("auth error: something alice")))
        assertEquals("error=PasswordLoginRejected status=none", loginFailureLogLine(PasswordLoginRejected()))
    }
}
