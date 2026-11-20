// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

/**
 * Tokens drawn while the account still exists, spent after it is gone.
 *
 * Delete account removes the push subscriptions only once auth has deleted
 * the account, and each `POST /push/unsubscribe` spends a `push-register`
 * token. Auth issues tokens to a session, so after the delete the pool can no
 * longer refill; the tokens are therefore taken out of the pool first
 * ([AnonTokens.reserve]) and handed out here, one per request, before falling back to
 * [fallback] (which may still hold pooled tokens). A token is verified by
 * push-gw on its own, with no account behind it.
 */
public class PrefetchedAnonTokens(
    tokens: List<PooledAnonToken>,
    private val fallback: AnonTokenSource,
) : AnonTokenSource {

    private val held: ArrayDeque<PooledAnonToken> = ArrayDeque(tokens)

    override val headerName: String get() = fallback.headerName

    /** Hand over more tokens (the ones [AnonTokens.reserve] drew). */
    public fun add(tokens: List<PooledAnonToken>) {
        synchronized(held) { held.addAll(tokens) }
    }

    override suspend fun take(context: AnonTokenContext): PooledAnonToken {
        val ready = synchronized(held) {
            val index = held.indexOfFirst { it.context == context }
            if (index >= 0) held.removeAt(index) else null
        }
        return ready ?: fallback.take(context)
    }

    override suspend fun takeDetached(session: AnonSession, context: AnonTokenContext): PooledAnonToken =
        fallback.takeDetached(session, context)

    override fun releaseSession() {
        synchronized(held) { held.clear() }
    }
}
