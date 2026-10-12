// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.job
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull

/**
 * The view models a test has open, and the one way to close them.
 *
 * ## Why a test cannot just drop a [ChatViewModel]
 *
 * The view model's constructor starts collectors in `viewModelScope` (the test
 * `Main`) that hop to `Dispatchers.Default` for the roster reads and come back,
 * and its actions hop to the repository's MLS thread and come back. Those hops
 * run on real threads. A test that returns while one is in flight races its own
 * `Dispatchers.resetMain()`: the resumption then asks a `Main` that no longer
 * exists whether it needs a dispatch, `MissingMainCoroutineDispatcher` throws
 * on the worker thread, and kotlinx-coroutines-test charges that exception to
 * whichever `runTest` starts next in the JVM (`UncaughtExceptionsBeforeTest`),
 * in a test that did nothing wrong.
 *
 * Cancelling the scope is not enough either. A block already running on
 * `Dispatchers.Default` is not interrupted by the cancellation; when it returns
 * it still resumes its caller, with the cancellation, through `Main`.
 *
 * So [leaveAllAndSettle] does what leaving the screens does in the app (the
 * store clears each view model, which cancels its scope) and then WAITS for
 * every scope to finish, while the test `Main` is still installed. Call it
 * before `Dispatchers.resetMain()` and before the repository is closed.
 */
internal class OpenChats {
    private val store = ViewModelStore()
    private val scopes = ArrayList<Job>()

    /** Register [viewModel] as an open screen and return it. */
    fun <T : ViewModel> open(viewModel: T): T {
        scopes += viewModel.viewModelScope.coroutineContext.job
        store.put("screen-${scopes.size}", viewModel)
        return viewModel
    }

    /**
     * Leave every open screen, as navigation does: each view model is cleared
     * and its scope cancelled. Work already on another thread may still be
     * running when this returns; [leaveAllAndSettle] is what waits for it.
     */
    fun leaveAll() {
        store.clear()
    }

    /**
     * [leaveAll], then block until every view model's scope has finished, so
     * nothing started through them can resume after this returns.
     */
    fun leaveAllAndSettle() {
        leaveAll()
        val settled = runBlocking { withTimeoutOrNull(SETTLE_TIMEOUT_MS) { scopes.joinAll() } }
        if (settled == null) {
            throw AssertionError(
                "view model scopes still running ${SETTLE_TIMEOUT_MS} ms after being cleared: " +
                    scopes.filterNot { it.isCompleted },
            )
        }
    }

    private companion object {
        /** Far above anything the un-bootstrapped repository takes; a scope
         *  still alive after this is stuck, and the test should say so. */
        const val SETTLE_TIMEOUT_MS: Long = 30_000
    }
}
