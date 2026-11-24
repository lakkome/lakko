// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive

/**
 * The claim an open chat screen holds on its group's polling
 * ([ChatRepository.claimScreenPoll]): while held, the app-level all-groups
 * poll leaves the group out, because the screen long-polls it itself.
 *
 * It tracks whether it is held so the screen can give it up while it is not
 * polling and take it back when it polls again, and release exactly once at
 * the end.
 */
internal class ScreenPollClaim(
    private val claim: () -> Unit,
    private val release: () -> Unit,
) {
    @Volatile
    var held: Boolean = false
        private set

    /** Take the claim if it is not held. */
    fun ensure() {
        if (held) return
        claim()
        held = true
    }

    /** Give the claim up if it is held. */
    fun giveUp() {
        if (!held) return
        held = false
        release()
    }
}

/**
 * The chat screen's receive loop, one long poll after another, for as long as
 * the calling coroutine is active.
 *
 * While this device's membership of the group has ended the loop does not
 * poll (the repository refuses the fetch at once, so a loop that kept calling
 * would spin) and it gives the claim up, so the app-level poll covers the
 * group if it comes back to life under a state this screen has not read yet.
 * It waits for the membership to resume and polls again.
 *
 * It used to `break` instead, keeping the claim: a person removed from a group
 * and added back while the conversation stayed open (or a screen that opened
 * on a state still reading as ended) left the group with no poller at all. The
 * app-level poll skipped it because the screen held it, the screen polled
 * nothing, and the newcomer's device read the rows of its new membership (the
 * group's name among them) only when something else happened to page the
 * journal, minutes later (seen on an emulator 2026-11-23).
 *
 * @param ended whether this device's membership of the group has ended.
 * @param poll one long poll; [GroupPollOutcome.UNAVAILABLE] returns at once,
 *   so it is followed by [backoff].
 */
internal suspend fun runScreenPollLoop(
    ended: StateFlow<Boolean>,
    claim: ScreenPollClaim,
    poll: suspend () -> GroupPollOutcome,
    backoff: suspend () -> Unit,
) {
    while (currentCoroutineContext().isActive) {
        if (ended.value) {
            claim.giveUp()
            ended.first { !it }
            continue
        }
        claim.ensure()
        try {
            if (poll() == GroupPollOutcome.UNAVAILABLE) backoff()
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            backoff()
        }
    }
}
