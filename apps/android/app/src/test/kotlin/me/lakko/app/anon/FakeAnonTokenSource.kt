// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.anon

/**
 * A token source for host tests: hands out `tok-1`, `tok-2`, ... per call and
 * records which contexts were drawn, or throws [failWith] when set.
 */
class FakeAnonTokenSource(
    override val headerName: String = "x-lakko-anon-token",
    var failWith: AnonTokenException? = null,
) : AnonTokenSource {
    val taken = mutableListOf<AnonTokenContext>()
    val detached = mutableListOf<Pair<AnonSession, AnonTokenContext>>()
    var released = 0
    private var next = 1

    override suspend fun take(context: AnonTokenContext): PooledAnonToken {
        failWith?.let { throw it }
        taken += context
        return PooledAnonToken(context, epoch = 1, headerValue = "tok-${next++}")
    }

    override suspend fun takeDetached(session: AnonSession, context: AnonTokenContext): PooledAnonToken {
        failWith?.let { throw it }
        detached += session to context
        return PooledAnonToken(context, epoch = 1, headerValue = "tok-${next++}")
    }

    override fun releaseSession() {
        released++
    }
}
