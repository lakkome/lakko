// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.BuildConfig

/**
 * SFU endpoint configuration for the Android client — the Kotlin twin of
 * the iOS `SfuConfig.swift` (Info.plist `LakkoSfuUrl`) and the web
 * `apps/web/src/calls/sfu-config.ts` (`VITE_LAKKO_SFU_URL`).
 *
 * The Rust SFU is a WebSocket origin (`ws://` / `wss://`) with no
 * trailing path; [SfuSignalingClient] appends `/v1/connect`. The value is
 * read from [BuildConfig.LAKKO_SFU_URL] (set per build via the gradle
 * property `lakko.sfu.url`). The SFU terminates WebSocket signaling + UDP
 * media directly, so it is a separate deployment from the `/api`
 * HTTP services.
 *
 * Returns `null` when unset / blank, so the group-call trigger refuses
 * with a clear log line rather than dialing a bogus endpoint (1x1 P2P
 * calls are unaffected).
 */
object SfuConfig {
    /** The configured SFU WebSocket origin, or `null` if unset. Trailing
     *  slashes are trimmed so callers can append `/v1/connect` cleanly. */
    val baseUrl: String?
        get() {
            val trimmed = BuildConfig.LAKKO_SFU_URL
                .trim()
                .trimEnd('/')
            return trimmed.ifEmpty { null }
        }
}
