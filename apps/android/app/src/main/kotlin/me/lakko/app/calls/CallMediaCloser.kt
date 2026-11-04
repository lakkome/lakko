// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.concurrent.Executor
import java.util.concurrent.Executors

/**
 * Where a call's peer connection and camera are closed: one thread, in the
 * order the calls ended, never the main thread. Closing either waits on
 * libwebrtc's own threads.
 */
internal object CallMediaCloser {

    @Volatile
    internal var executor: Executor = Executors.newSingleThreadExecutor { work ->
        Thread(work, "lakko-call-close").apply { isDaemon = true }
    }

    /** Run [work]; a failure is logged and the next close still runs. */
    fun close(work: () -> Unit) {
        executor.execute {
            try {
                work()
            } catch (cause: Throwable) {
                android.util.Log.w("CallMediaCloser", "closing call media failed: ${cause.javaClass.simpleName}")
            }
        }
    }
}
