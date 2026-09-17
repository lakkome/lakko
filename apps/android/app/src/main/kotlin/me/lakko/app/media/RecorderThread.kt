// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import java.util.concurrent.Executors

/**
 * The one thread every [VoiceRecorder] call runs on.
 *
 * `MediaRecorder.prepare` / `start` / `stop` are binder calls into the media
 * server and can block for seconds; on the main thread that froze input and
 * lost the release of a hold-to-record gesture (an ANR). A single thread,
 * rather than `Dispatchers.IO`, keeps the calls for one take strictly ordered:
 * a stop queued while start is still running waits for it instead of racing
 * it.
 *
 * [scope] is process-wide on purpose: a take released or discarded as the
 * chat screen closes still gets its recorder stopped and its file removed
 * after the screen's own scope is gone.
 */
public object RecorderThread {
    public val dispatcher: CoroutineDispatcher = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "lakko-recorder").apply { isDaemon = true }
    }.asCoroutineDispatcher()

    public val scope: CoroutineScope = CoroutineScope(SupervisorJob() + dispatcher)
}
