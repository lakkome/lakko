// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// SFU endpoint configuration for the iOS client — the analog of the
/// web `apps/web/src/calls/sfu-config.ts` (`VITE_LAKKO_SFU_URL`).
///
/// The Rust SFU is a WebSocket origin (`ws://` / `wss://`) with no
/// trailing path; `SfuSignalingClient` appends `/v1/connect`. The URL is
/// read from the app's `Info.plist` key `LakkoSfuUrl` (set per build
/// configuration). The SFU terminates WebSocket signaling + UDP media
/// directly, so it is a separate deployment from the `/api/*` services.
///
/// Returns `nil` when unset / blank, so the group-call trigger refuses
/// with a clear message rather than dialing a bogus endpoint (1×1 P2P
/// calls are unaffected).
enum SfuConfig {
    /// The configured SFU WebSocket origin, or `nil` if unset. Trailing
    /// slashes are trimmed so callers can append `/v1/connect` cleanly.
    static var baseUrl: String? {
        guard
            let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoSfuUrl") as? String
        else {
            return nil
        }
        var trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix("/") {
            trimmed.removeLast()
        }
        return trimmed.isEmpty ? nil : trimmed
    }
}
