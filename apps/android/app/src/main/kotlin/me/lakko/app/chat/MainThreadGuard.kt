// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.os.Looper
import me.lakko.app.BuildConfig

/**
 * Debug-only tripwire for the repository's MLS / FFI / persistence regions.
 *
 * The UniFFI `Group` and `Identity` calls are synchronous JNA calls into the
 * Rust core — a decrypt, a commit, a `snapshotStorage` of the whole provider,
 * a Keystore-wrapped file write. Run on the main thread from a Compose
 * `LaunchedEffect` they held it for seconds and produced an ANR inside
 * `Group.ownCommitterRank` / `Group.members` / `Group.pendingProposals`
 * reached from the chat screen's poll loop. `ChatRepository` now hops every
 * such region onto its own dispatcher; this guard is what makes a regression
 * fail LOUDLY in a debug build instead of quietly as jank.
 *
 * Inert in release builds. [isMainThread] is replaceable so a host unit test
 * can prove the guard fires without a `Looper`; the default swallows the
 * "not mocked" stub failure the plain android.jar throws off-device, so the
 * pure-Kotlin tests that exercise repository helpers keep running.
 */
internal object MainThreadGuard {
    @Volatile
    var isMainThread: () -> Boolean = ::defaultIsMainThread

    /** Fail (debug builds only) when called on the main thread. */
    fun assertOffMain(what: String) {
        if (!BuildConfig.DEBUG) return
        val onMain = try {
            isMainThread()
        } catch (_: Throwable) {
            // No Android runtime (host unit tests): nothing to check against.
            return
        }
        check(!onMain) { "$what must not run on the main thread" }
    }

    private fun defaultIsMainThread(): Boolean =
        Looper.getMainLooper()?.isCurrentThread == true
}
