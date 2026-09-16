// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#if DEBUG
import Foundation
import os

/// Debug-only counter of SwiftUI `body` evaluations for the shell views.
///
/// Every counted `body` calls ``tick(_:)`` as its first statement, which
/// bumps a per-view counter and writes one unified-log line
/// (`subsystem me.lakko.ui`, `category body-eval`, debug level), so a live
/// run can be measured with
/// `xcrun simctl spawn booted log stream --level debug --predicate 'subsystem == "me.lakko.ui"'`
/// and a hosted test can read ``count(of:)``. Compiled out of Release.
@MainActor
enum BodyEvaluationCounter {
    private static let log = Logger(subsystem: "me.lakko.ui", category: "body-eval")

    private(set) static var counts: [String: Int] = [:]

    /// Record one evaluation of `view`'s body.
    static func tick(_ view: String) {
        let next = (counts[view] ?? 0) + 1
        counts[view] = next
        log.debug("body \(view, privacy: .public) #\(next, privacy: .public)")
    }

    static func count(of view: String) -> Int {
        counts[view] ?? 0
    }

    static func reset() {
        counts = [:]
    }
}
#endif
