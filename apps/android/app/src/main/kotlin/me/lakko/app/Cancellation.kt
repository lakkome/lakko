// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive

/**
 * First statement of a `catch (e: Throwable)` that reports a failed request.
 *
 * A coroutine cancelled because the person left the screen (the scope was
 * cleared, the job was replaced) is not a failed request: there is nothing to
 * show, nothing to log as a failure and nothing to retry. This passes the
 * cancellation on and returns only for a real failure, so the code after it
 * runs exactly as it did before.
 *
 * Two things count as cancelled:
 *
 *  - [e] is a [CancellationException]. A [TimeoutCancellationException] is the
 *    exception: a `withTimeout` that ran out is a real failure of the request
 *    and stays one.
 *  - the calling coroutine is no longer active, whatever [e] is. Lower layers
 *    wrap what they catch (`throw ChatError(..., e)`, a `Failed` result), so a
 *    cancellation routinely arrives here as an ordinary exception. Asking the
 *    coroutine rather than the exception type is what makes the check hold
 *    without every layer below having to preserve the type.
 *
 * [release] runs only on the cancelled path, before the rethrow. It is for the
 * busy flag or the lock the `catch` would have released on a real failure:
 * cancellation is passed on, but nothing is left held.
 */
internal suspend inline fun rethrowIfCancelled(e: Throwable, release: () -> Unit = {}) {
    val context = currentCoroutineContext()
    val cancellation = e is CancellationException && e !is TimeoutCancellationException
    if (!cancellation && context.isActive) return
    release()
    if (cancellation) throw e
    context.ensureActive()
}
