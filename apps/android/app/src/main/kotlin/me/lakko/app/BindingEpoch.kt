// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableJob
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/**
 * "Is the account this work was started for still the one that is bound?"
 *
 * The repositories and singletons of this app live for the whole process:
 * they are not rebuilt on sign-out, they are told the session ended and clear
 * their state. Their scopes live as long, so work one of them started for
 * account A (a fetch, a publish, a restore) keeps running after A signed out,
 * and the code after its suspension writes into whatever the owner holds by
 * then: nothing, or account B's state.
 *
 * The owner of a binding (the auth repository for the signed-in account, a
 * repository for the account it was bootstrapped with) calls [advance]
 * whenever that binding ends: sign-out, forget device, a landing for another
 * account. Work uses it one of two ways:
 *
 *  - [capture] when it starts and [Binding.isCurrent] / [Binding.ensureCurrent]
 *    after every suspension, before it writes or acts. For work whose
 *    coroutine belongs to somebody else (a screen, a caller).
 *  - [launchBound] for fire-and-forget work the owner launches itself: the
 *    coroutine is a child of the binding and is cancelled with it, so it stops
 *    at its next suspension and never reaches the code after it.
 *
 * A counter and not the account id alone: signing out and back in to the SAME
 * account is a new binding too, with new handles, and work from the first must
 * not complete into the second. Mirror of `apps/web/src/auth/binding-epoch.ts`.
 */
public class BindingEpoch {
    private val lock = Any()
    private var epoch: Long = 0
    private var parent: CompletableJob = SupervisorJob()

    /**
     * The binding ended. Every [Binding] captured before this call answers
     * `false` from now on, and every coroutine started with [launchBound]
     * before it is cancelled.
     */
    public fun advance() {
        val ended = synchronized(lock) {
            epoch += 1
            parent.also { parent = SupervisorJob() }
        }
        ended.cancel(StaleBindingException())
    }

    /** Remember the binding that is current now. */
    public fun capture(): Binding {
        val mine = synchronized(lock) { epoch }
        return Binding { synchronized(lock) { epoch } == mine }
    }

    /** The parent of the coroutines that belong to the current binding. */
    internal fun job(): Job = synchronized(lock) { parent }
}

/** One captured binding of a [BindingEpoch]. */
public class Binding internal constructor(private val current: () -> Boolean) {
    /** Whether the binding this was captured under is still the bound one. */
    public val isCurrent: Boolean get() = current()

    /**
     * Pass when the binding is still current; otherwise stop the calling work
     * with a [StaleBindingException], which is a cancellation: there is
     * nothing to report, nothing to log as a failure and nothing to retry
     * (see [rethrowIfCancelled]).
     */
    public fun ensureCurrent() {
        if (!current()) throw StaleBindingException()
    }
}

/** The account this work was started for is no longer the bound one. */
public class StaleBindingException :
    CancellationException("the session this work was started for has ended")

/**
 * Launch [block] in this scope (its dispatcher, its exception handling) as
 * work of [epoch]'s current binding: cancelled when that binding ends instead
 * of when the scope does. A coroutine started with [CoroutineStart.ATOMIC]
 * still runs up to its first suspension, so a `finally` that wipes a secret
 * always runs.
 */
public fun CoroutineScope.launchBound(
    epoch: BindingEpoch,
    start: CoroutineStart = CoroutineStart.DEFAULT,
    block: suspend CoroutineScope.() -> Unit,
): Job = launch(context = epoch.job(), start = start, block = block)
