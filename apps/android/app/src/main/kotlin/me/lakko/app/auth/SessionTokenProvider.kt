// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * ADR-0073 section 3.3 — the ONE token provider every authenticated client
 * goes through. Mirror of the web `SessionTokenProvider` and the iOS one.
 *
 * The clients keep handing their requests the access token they captured
 * ([IdentitySecrets.sessionToken], a `sessionTokenProvider` closure); the
 * transport layer ([me.lakko.app.transport.SessionBearerEngine]) asks this
 * provider for the CURRENT token of that account before the request goes
 * out, and after a `401` for a refreshed one, so no client re-implements the
 * refresh and a stale captured token is never what reaches the wire.
 *
 * The rules, each pinned by a test:
 *
 *   * [current] hands out the access token while more than
 *     [SESSION_REFRESH_AHEAD_MS] remain, else refreshes first.
 *   * ONE refresh in flight: callers queue on [refreshMutex], and a caller
 *     that queued behind a refresh finds the pair already replaced and sends
 *     nothing ([afterUnauthorized] compares the token it used with the one
 *     installed).
 *   * A refresh answered `key-generation-superseded` reports
 *     [SessionRefusal.KeysReset] ONCE and keeps the stale pair: the device is
 *     about to wipe, and requests that are still in flight may fail as they
 *     like. A refresh answered `device-revoked` (ADR-0074) reports
 *     [SessionRefusal.DeviceRevoked] the same way.
 *   * A refresh answered `refresh-invalid` reports
 *     [SessionRefusal.RefreshInvalid] and drops the pair: the device goes to
 *     the sign-in screen, nothing is wiped.
 *   * A refresh that could not be made (offline, 5xx) keeps the current
 *     token; the caller fails as it did before a refresh existed.
 *   * A refresh answered `403 account-suspended` (ADR-0089 section 4)
 *     reports the restricted credentials it carries to [install]'s
 *     `onSuspended` and marks the session suspended: from then on the
 *     provider refreshes nothing by itself (the suspension controller asks
 *     the refresh route on its own schedule), and a plain `401` is not
 *     retried. A later refresh that brings a normal pair reports
 *     `onResumed`.
 *
 * Persistence is the installer's ([Installed.persist] runs with every pair
 * the refresh brings), so the pair lands where the session token always was.
 */
public class SessionTokenProvider(
    private val refresher: SessionRefresher,
    private val clock: () -> Long = { System.currentTimeMillis() },
    private val log: (String) -> Unit = {},
) {
    /** What `POST /auth/session/refresh` is to the provider. */
    public interface SessionRefresher {
        /** The new pair, or throws [AuthHttpError] / [AuthNetworkError]. */
        public suspend fun refresh(refreshToken: String): SessionPair
    }

    private class Installed(
        val accountSegment: String?,
        @Volatile var pair: SessionPair,
        val persist: (SessionPair) -> Unit,
        val onRefusal: (SessionRefusal) -> Unit,
        val onSuspended: (SuspendedCredentials) -> Unit,
        val onResumed: (SessionPair) -> Unit,
        /** Set once a refusal was reported, so it is reported once. */
        @Volatile var refused: Boolean = false,
        /** ADR-0089 - the account is suspended; the pair is the `s1` one. */
        @Volatile var suspended: Boolean = false,
    )

    private val refreshMutex = Mutex()

    @Volatile
    private var installed: Installed? = null

    /** How many refresh requests were sent (tests). */
    @Volatile
    public var refreshCount: Int = 0
        private set

    /**
     * Install [pair] as the session this provider serves. [persist] runs
     * with every pair a refresh brings; [onRefusal] with the one refusal a
     * refresh can answer. Replaces whatever was installed.
     */
    public fun install(
        pair: SessionPair,
        persist: (SessionPair) -> Unit,
        onRefusal: (SessionRefusal) -> Unit,
        onSuspended: (SuspendedCredentials) -> Unit = {},
        onResumed: (SessionPair) -> Unit = {},
    ) {
        installed = Installed(
            accountSegment = accountSegmentOfSessionToken(pair.accessToken),
            pair = pair,
            persist = persist,
            onRefusal = onRefusal,
            onSuspended = onSuspended,
            onResumed = onResumed,
            suspended = pair.accessToken.startsWith(RESTRICTED_TOKEN_PREFIX),
        )
    }

    /**
     * ADR-0089 section 4 - serve the restricted pair of [credentials] (the
     * `s1` in the access slot, the `r2` in the refresh slot) as the
     * suspended session of the installed account. A no-op when nothing is
     * installed or the credentials name another account.
     */
    public fun serveSuspended(credentials: SuspendedCredentials) {
        val inst = installed ?: return
        val pair = credentials.asSessionPair()
        if (accountSegmentOfSessionToken(pair.accessToken) != inst.accountSegment) return
        inst.pair = pair
        inst.suspended = true
    }

    /** Whether the installed session is suspended. */
    public fun isSuspended(): Boolean = installed?.suspended == true

    /**
     * ADR-0089 - a bearer route of auth answered `403 account-suspended`
     * with no credentials: refresh once to get them (the refresh route
     * answers the same `403` with the restricted session, which reaches
     * `onSuspended`). A no-op once the session is known to be suspended.
     */
    public suspend fun suspendedFromBearerRoute() {
        val inst = installed ?: return
        if (inst.suspended || inst.refused) return
        refreshIfStillCurrent(inst.pair.accessToken)
    }

    /** A landing (or a reset) handed the same account a newer pair. Keeps
     *  the installed persist and refusal hooks; a no-op when nothing is
     *  installed or the pair names another account. */
    public fun update(pair: SessionPair) {
        val inst = installed ?: return
        if (accountSegmentOfSessionToken(pair.accessToken) != inst.accountSegment) return
        inst.pair = pair
        inst.refused = false
        inst.suspended = pair.accessToken.startsWith(RESTRICTED_TOKEN_PREFIX)
    }

    /**
     * ADR-0074 - `bind-device` replaced the installed session's pair with
     * one that names this device: serve it and persist it like a refreshed
     * pair. A no-op when nothing is installed or the pair names another
     * account.
     */
    public fun adopt(pair: SessionPair) {
        val inst = installed ?: return
        if (accountSegmentOfAccessToken(pair.accessToken) != inst.accountSegment) return
        inst.pair = pair
        try {
            inst.persist(pair)
        } catch (cause: Throwable) {
            log("session pair persist after bind failed: ${cause.javaClass.simpleName}")
        }
    }

    /** Forget the session (logout, wipe). */
    public fun clear() {
        installed = null
    }

    /** The installed pair, without refreshing. */
    public fun peek(): SessionPair? = installed?.pair

    /**
     * A bearer route of auth or discovery (not the refresh route) answered
     * `key-generation-superseded`: the same consequence as the refresh
     * refusal, reported once per installed session. A no-op when nothing is
     * installed, the session was already refused, or [refused] is not the
     * token held now: a late answer to a replaced token says nothing about
     * the session, and acting on it wipes a device that just rotated its keys.
     */
    public fun reportSupersededFromBearerRoute(refused: String) {
        val inst = installed ?: return
        if (inst.refused || inst.pair.accessToken != refused) return
        inst.refused = true
        inst.onRefusal(SessionRefusal.KeysReset)
    }

    /**
     * ADR-0074 - a bearer route of auth answered `401 device-revoked`:
     * another device of the account removed this one. Reported once per
     * installed session, like the refresh refusal.
     */
    public fun reportDeviceRevokedFromBearerRoute() {
        val inst = installed ?: return
        if (inst.refused) return
        inst.refused = true
        inst.onRefusal(SessionRefusal.DeviceRevoked)
    }

    /** Whether [bearer] is an access token of the account this provider
     *  serves - the test the transport runs before substituting it. */
    public fun manages(bearer: String): Boolean {
        val inst = installed ?: return false
        val segment = accountSegmentOfAccessToken(bearer) ?: return false
        return segment == inst.accountSegment
    }

    /**
     * The access token to send now: the installed one while it has more than
     * [SESSION_REFRESH_AHEAD_MS] left, else the one a refresh brings. On a
     * refresh that could not be made the installed (possibly expired) token
     * is returned, so the request fails as it would have without a provider.
     * `null` when no session is installed.
     */
    public suspend fun current(): String? {
        val inst = installed ?: return null
        val pair = inst.pair
        // Suspended: nothing to refresh to; the restricted session is served
        // as it is (the controller keeps it live).
        if (inst.suspended) return pair.accessToken
        if (tokenActionAt(pair.accessExpiresAtMs, clock()) == TokenAction.Use) return pair.accessToken
        return refreshIfStillCurrent(pair.accessToken)?.accessToken ?: installed?.pair?.accessToken
    }

    /**
     * A bearer route answered a plain `401` for [usedToken]: refresh once and
     * return the token to retry with, or `null` when there is nothing to
     * retry with (no session, the refresh failed, or the session is gone).
     * A caller whose [usedToken] is already replaced gets the replacement
     * without a second refresh.
     */
    public suspend fun afterUnauthorized(usedToken: String): String? {
        val inst = installed ?: return null
        if (inst.suspended) return null
        return refreshIfStillCurrent(usedToken)?.accessToken
    }

    /** Refresh now, whatever the expiry (the silent restore). The new pair,
     *  or `null` when the refresh did not yield one. */
    public suspend fun refreshNow(): SessionPair? {
        val inst = installed ?: return null
        return refreshIfStillCurrent(inst.pair.accessToken)
    }

    /**
     * Under the one refresh lock: when the installed access token is still
     * [staleAccess], refresh; when another caller already replaced it, hand
     * that pair back. Returns `null` when the refresh failed or was refused.
     */
    private suspend fun refreshIfStillCurrent(staleAccess: String): SessionPair? =
        refreshMutex.withLock {
            val inst = installed ?: return@withLock null
            if (inst.pair.accessToken != staleAccess) return@withLock inst.pair
            if (inst.refused) return@withLock null
            val sent = inst.pair
            val fresh = try {
                refreshCount += 1
                refresher.refresh(sent.refreshToken)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (err: AuthHttpError) {
                // [update] replaced the pair while this was out: the refusal
                // is of the old pair, not of the session.
                if (inst.pair !== sent) return@withLock inst.pair
                // ADR-0089 section 4 - the account is suspended: the answer
                // carries the restricted session. Not a sign-out, not a wipe.
                val suspension = err.suspended
                if (suspension != null &&
                    accountSegmentOfSessionToken(suspension.restrictedToken) == inst.accountSegment
                ) {
                    log("session refresh answered: the account is suspended")
                    inst.pair = suspension.asSessionPair()
                    inst.suspended = true
                    inst.onSuspended(suspension)
                    return@withLock null
                }
                when (sessionRefusalOf(err.status, err.code)) {
                    SessionRefusal.KeysReset -> {
                        log("session refresh refused: the account keys were reset elsewhere")
                        inst.refused = true
                        inst.onRefusal(SessionRefusal.KeysReset)
                    }
                    SessionRefusal.DeviceRevoked -> {
                        // ADR-0074 "T3 as built" item 1: auth reads the
                        // generation BEFORE the device floor, so this answer
                        // only ever reaches a credential of the CURRENT
                        // generation. A removed device whose account was
                        // also reset is told `key-generation-superseded`
                        // (the staged wipe and a recall record), never this.
                        // Nothing here depends on that order: each code is
                        // acted on as it arrives.
                        log("session refresh refused: this device was removed from the account")
                        inst.refused = true
                        inst.onRefusal(SessionRefusal.DeviceRevoked)
                    }
                    SessionRefusal.RefreshInvalid, SessionRefusal.Unauthorized -> {
                        log("session refresh refused: the refresh credential is not accepted")
                        inst.refused = true
                        if (installed === inst) installed = null
                        inst.onRefusal(SessionRefusal.RefreshInvalid)
                    }
                    null -> log("session refresh failed: ${err.status}")
                }
                return@withLock null
            } catch (err: AuthNetworkError) {
                log("session refresh could not be made: ${err.cause?.javaClass?.simpleName}")
                return@withLock null
            }
            val wasSuspended = inst.suspended
            inst.pair = fresh
            inst.suspended = false
            try {
                inst.persist(fresh)
            } catch (cause: Throwable) {
                log("session pair persist after refresh failed: ${cause.javaClass.simpleName}")
            }
            if (wasSuspended) inst.onResumed(fresh)
            fresh
        }
}

/**
 * The provider the process serves its requests from (the transport reads it
 * per request). `AuthRepository` installs one at construction; a test that
 * drives the transport installs its own.
 */
public object SessionTokens {
    @Volatile
    public var provider: SessionTokenProvider? = null

    /** Report a `key-generation-superseded` seen on a bearer route of auth
     *  or discovery (not on the refresh route): the same consequence as the
     *  refresh refusal, reported once. [refused] is the token the answer
     *  was to. */
    public fun reportSuperseded(refused: String) {
        provider?.reportSupersededFromBearerRoute(refused)
    }

    /** Report a `401 device-revoked` seen on a bearer route of auth. */
    public fun reportDeviceRevoked() {
        provider?.reportDeviceRevokedFromBearerRoute()
    }
}
