// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import androidx.credentials.exceptions.CreateCredentialUnknownException
import androidx.credentials.exceptions.domerrors.InvalidStateError
import androidx.credentials.exceptions.domerrors.NotAllowedError
import androidx.credentials.exceptions.publickeycredential.CreatePublicKeyCredentialDomException
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A provider that already holds a passkey for this account refuses with
 * WebAuthn `InvalidStateError` (the excluded-credential case). That is
 * "This device already has a passkey for this account." (UX 10.2), never
 * "cannot create passkeys"; every other refusal stays what it was.
 */
class PasskeyAlreadyExistsTest {

    @Test
    fun `an InvalidStateError refusal means the device already has a passkey for the account`() {
        val refusal = CreatePublicKeyCredentialDomException(InvalidStateError(), "excluded")
        val mapped = registrationFailure(refusal)
        assertTrue(mapped is PasskeyAlreadyExistsError)
        assertSame(refusal, mapped.cause)
    }

    @Test
    fun `other refusals are returned unchanged`() {
        val notAllowed = CreatePublicKeyCredentialDomException(NotAllowedError(), "no")
        assertSame(notAllowed, registrationFailure(notAllowed))
        val unknown = CreateCredentialUnknownException("unknown")
        assertSame(unknown, registrationFailure(unknown))
        val other = IllegalStateException("x")
        assertSame(other, registrationFailure(other))
    }
}
