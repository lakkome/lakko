// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryHttpError

/**
 * The username half of the discovery directory, as the auth flows use it.
 *
 * ## The defect this closes
 *
 * `DiscoveryClient.registerUsername` had no call site on Android. A user who
 * signed up here was never bound to their `@username`, so nobody could find
 * them to add them as a contact or to a group, and they could not sign in on a
 * second device, because every username-keyed flow resolves through
 * `/discovery/lookup` and got a 404. Login made the second half worse: it
 * refused outright when the device held no cached account, where web resolves
 * the typed username through discovery.
 *
 * Narrowed to an interface so the claim rule below (auth handle first, then
 * discovery; what a taken name looks like to the user) is unit-testable. The
 * production implementation drives the OPRF flow, which needs the native core.
 *
 * ## No lookup before a session (ADR-0054 S5)
 *
 * Sign-in and recovery no longer resolve a name here: a username lookup spends
 * an anonymous token, a token needs a session, and the auth routes now take
 * the login handle of the name instead ([resolveNamedSignIn]). This interface
 * therefore has no lookup at all, so no pre-session flow can reach one.
 *
 * ## Normalisation
 *
 * Nothing here normalises a username. The canonical form (trim, strip a
 * leading `@`, NFKC, lowercase) is computed inside the native OPRF blinder
 * `DiscoveryClient` already uses, and handed back as the normalised username;
 * a second Kotlin normaliser could only drift from the Rust one.
 */
internal interface AccountDirectory {
    /**
     * Bind [username] to [accountId] under [sessionToken] (the server refuses a
     * body account id that does not match the token's).
     *
     * @return the normalised username the server actually bound.
     * @throws DiscoveryHttpError with `status == 409` when the name is taken.
     */
    suspend fun registerUsername(sessionToken: String, accountId: ByteArray, username: String): String

    /**
     * ADR-0076: replace the account's current name with [username]
     * (`POST /discovery/username/replace`); the claimed name and the new
     * certificate. `409 username-taken` / `no-username`, `429 rename-too-soon`.
     */
    suspend fun replaceUsername(sessionToken: String, accountId: ByteArray, username: String): ClaimedUsername =
        throw UnsupportedOperationException("this directory cannot rename")

    /**
     * [registerUsername], also handing back the username certificate the
     * register call returned (ADR-0060 section 5), when it returned one.
     */
    suspend fun registerUsernameWithCertificate(
        sessionToken: String,
        accountId: ByteArray,
        username: String,
    ): ClaimedUsername = ClaimedUsername(registerUsername(sessionToken, accountId, username), null)
}

/** A claimed name and the certificate discovery issued for it. */
internal class ClaimedUsername(val username: String, val certificate: ByteArray?)

/** [AccountDirectory] over a short-lived [DiscoveryClient] per call. */
internal class DiscoveryAccountDirectory(
    private val clientFactory: () -> DiscoveryClient,
) : AccountDirectory {
    override suspend fun registerUsername(
        sessionToken: String,
        accountId: ByteArray,
        username: String,
    ): String = registerUsernameWithCertificate(sessionToken, accountId, username).username

    override suspend fun registerUsernameWithCertificate(
        sessionToken: String,
        accountId: ByteArray,
        username: String,
    ): ClaimedUsername = clientFactory().use { client ->
        // A fresh client per call, so the bearer is never left attached to a
        // client some other flow reuses.
        client.setSessionToken(sessionToken)
        // ADR-0054 - the OPRF evaluation spends a `discovery-lookup` token.
        // Signup claims the name before its session is installed, so the
        // token is drawn with this session explicitly.
        client.useTokenSession(
            me.lakko.app.anon.AnonSession(
                accountKey = base64Encode(accountId),
                bearer = sessionToken,
            ),
        )
        val result = client.registerUsername(accountId, username)
        ClaimedUsername(result.normalisedUsername, result.usernameCertificate)
    }

    override suspend fun replaceUsername(
        sessionToken: String,
        accountId: ByteArray,
        username: String,
    ): ClaimedUsername = clientFactory().use { client ->
        client.setSessionToken(sessionToken)
        client.useTokenSession(
            me.lakko.app.anon.AnonSession(accountKey = base64Encode(accountId), bearer = sessionToken),
        )
        val result = client.replaceUsername(accountId, username)
        ClaimedUsername(result.normalisedUsername, result.usernameCertificate)
    }
}

/**
 * Claim [username] for the account signup just created. Mirror of the web
 * `signup` (`apps/web/src/auth/AuthContext.tsx`), which does this right after
 * the OPAQUE registration commits and before anything is persisted locally.
 *
 * ## Claim order (ADR-0054 S5, load-bearing)
 *
 * The login handle at auth and the name at discovery are two first-come
 * registries that cannot check each other, so an honest client claims in one
 * order:
 *
 *   1. bind the handle at auth (`PUT /auth/login-handle`); a `409` means the
 *      name is taken and discovery is never asked; a `403
 *      registration-closed` (the beta is full) stops here as
 *      [RegistrationClosed]; a `429 handle-bind-limited`
 *      stops here the same way, propagated as the [AuthHttpError] whose copy
 *      is [handleBindLimitedCopy] (the form keeps its contents);
 *   2. register the name at discovery; if that fails (taken or otherwise) the
 *      handle is released again (`DELETE /auth/login-handle`), so the name is
 *      not left held at auth by an account that never got it.
 *
 * A taken name becomes an [AuthClientArgError] carrying the sentence the form
 * shows; every other failure propagates unchanged, so a network error still
 * reads as one. Either way signup fails rather than landing a session nobody
 * can find — the account then exists server-side with no username, the same
 * posture web accepts (the user retries with another name).
 *
 * @return the normalised username to persist and display.
 */
internal suspend fun claimUsernameAtSignup(
    directory: AccountDirectory,
    binder: LoginHandleBinder,
    handles: LoginHandleSource,
    sessionToken: String,
    accountId: ByteArray,
    username: String,
    /** ADR-0097: a held supporter place for the first bind, or `null`. */
    placeToken: String? = null,
): String = claimUsername(directory, binder, handles, sessionToken, accountId, username, placeToken).username

/**
 * The claim itself, in the order above, keeping the certificate the register
 * call returned. Also what a guest runs later ("Choose a username").
 */
internal suspend fun claimUsername(
    directory: AccountDirectory,
    binder: LoginHandleBinder,
    handles: LoginHandleSource,
    sessionToken: String,
    accountId: ByteArray,
    username: String,
    /** ADR-0097: a held supporter place for the first bind, or `null`. */
    placeToken: String? = null,
): ClaimedUsername {
    val trimmed = username.trim()
    val named = namedHandleOrRefuse(handles, trimmed)
    val binding = if (placeToken != null) {
        binder.bindHoldingPlace(sessionToken, named.handle, placeToken)
    } else {
        binder.bind(sessionToken, named.handle)
    }
    when (binding) {
        LoginHandleBinding.Bound -> Unit
        LoginHandleBinding.Taken ->
            throw AuthClientArgError(takenCopy(trimmed), me.lakko.app.l10n.UiText("auth.signup.error.username_taken"))
        // The beta is full (`docs/ux/beta-account-cap.md`): nothing was bound
        // at auth, and discovery is never asked, so no name is left anywhere.
        LoginHandleBinding.Closed -> throw RegistrationClosed()
        // ADR-0097: the place this bind carried was spent by another account.
        LoginHandleBinding.SupporterCodeUsed -> throw SupporterCodeUsed()
    }
    try {
        return directory.registerUsernameWithCertificate(sessionToken, accountId, trimmed)
    } catch (err: Throwable) {
        // The name did not land at discovery: give the handle back. A failed
        // release must not mask the reason the claim failed.
        try {
            binder.unbind(sessionToken)
        } catch (release: Throwable) {
            err.addSuppressed(release)
        }
        // ADR-0076: the 409 body is JSON now. `username-taken` (or the
        // older bodiless 409) is the taken line; `username-already-set`
        // (the account already holds a name) is not, and propagates.
        if (isUsernameTaken(err)) {
            throw AuthClientArgError(takenCopy(trimmed), me.lakko.app.l10n.UiText("auth.signup.error.username_taken"))
        }
        throw err
    }
}

private fun takenCopy(trimmed: String): String = "username @$trimmed is already taken — pick another"

