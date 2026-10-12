// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull

/**
 * End a view model a test built, and WAIT until every coroutine of its
 * `viewModelScope` has finished. Call it before `Dispatchers.resetMain()`.
 *
 * ## Why a test must do this
 *
 * `ChatViewModel`'s constructor starts collectors that hop to
 * `Dispatchers.Default` (and to the repository's MLS thread) and resume on
 * `Main`. A test that returns while such a hop is still running, and then
 * resets `Main`, leaves a real thread about to resume onto a `Main` that no
 * longer exists: "Module with the Main dispatcher had failed to initialize",
 * raised inside the coroutine machinery, where nothing can catch it. It
 * reaches the global handler, and `runTest` charges it to whichever test
 * starts next as `UncaughtExceptionsBeforeTest` - for a long time that was
 * `OneToOneCallSurvivesChatScreensTest`, the first `runTest` after the view
 * model tests in this package, which failed about once in several runs.
 *
 * Cancelling is not enough: a cancelled continuation still asks its
 * dispatcher whether a dispatch is needed when the hop returns. So the scope
 * is cancelled (through the store, as the framework does it, so `onCleared`
 * runs) and then JOINED while the test `Main` is still installed.
 */
internal fun ViewModel.clearAndJoin(timeoutMs: Long = 10_000L) {
    val job = viewModelScope.coroutineContext[Job]
    if (job == null || job.isActive) {
        ViewModelStore().also { it.put("under-test", this) }.clear()
    }
    if (job != null) {
        val finished = runBlocking { withTimeoutOrNull(timeoutMs) { job.join() } }
        check(finished != null) { "a view model's coroutines did not finish within $timeoutMs ms" }
    }
}
