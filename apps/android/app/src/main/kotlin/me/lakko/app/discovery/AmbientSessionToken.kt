// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

/**
 * The signed-in session's bearer token, readable from any thread, for one
 * purpose only: keying `services/discovery`'s rate limiter.
 *
 * Discovery charges `/discovery/oprf`, `/discovery/lookup` and
 * `/discovery/channel/lookup` to the **account** when the request carries a
 * valid session token and to the source **IP** otherwise, so everyone behind
 * one NAT shares a single anonymous bucket. Most `DiscoveryClient()` call
 * sites are constructed without a token, so without this holder a signed-in
 * user's lookups were charged to that shared bucket.
 *
 * [DiscoveryClient] attaches this token to those three rate-limited reads
 * when the client itself carries none. It is never used for a protected
 * write: those still require the token set on the client explicitly, so an
 * anonymous client cannot write as whichever account happens to be signed
 * in. The server reads the token on those three routes only to pick the
 * bucket; an expired token falls back to the IP bucket, never to a refusal.
 *
 * `AuthRepository` installs a provider reading its `identitySecrets` when it
 * is constructed, so every sign-in, restore and logout is reflected with no
 * per-write bookkeeping; an activity recreate constructs a new repository,
 * whose provider replaces the old one. `null` before sign-in, so signup and
 * recovery lookups stay anonymous as they must.
 */
public object AmbientSessionToken {

    @Volatile
    private var provider: () -> String? = { null }

    /** The current token, or `null` when signed out. */
    public val current: String?
        get() = provider()?.takeIf { it.isNotEmpty() }

    /** Replace the token source. */
    public fun install(source: () -> String?) {
        provider = source
    }
}
