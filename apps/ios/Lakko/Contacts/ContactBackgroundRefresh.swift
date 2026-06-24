// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import BackgroundTasks
import Foundation

/// Background contact-inbox refresh scheduling (G-29).
///
/// The contact inbox is otherwise foreground-poll only — a long-poll loop
/// driven by ``ContactsScreen``'s `.task { … }` modifier — so a new contact
/// request that arrives while the app is backgrounded isn't surfaced until the
/// user reopens the app. This type schedules a `BGAppRefreshTask` so iOS wakes
/// the app periodically to run one quick inbox fetch; the matching handler is
/// registered on the scene via `.backgroundTask(.appRefresh(_:))` in
/// ``LakkoApp``.
///
/// This is opportunistic, not guaranteed delivery: iOS decides if and when to
/// run an app-refresh task based on usage patterns, battery, and network. It
/// narrows the "missed a request while backgrounded" window; a push-driven
/// (APNs `content-available`) wake for immediate surfacing is a larger
/// follow-up that needs the push gateway to emit a silent push on inbox
/// arrival.
///
/// The identifier must match the `BGTaskSchedulerPermittedIdentifiers` entry in
/// `Info.plist` (and the app must declare the `fetch` `UIBackgroundMode`), or
/// the OS refuses to launch the task.
public enum ContactBackgroundRefresh {
    /// BGTask identifier — reverse-DNS under the app bundle id, mirrored in
    /// `Info.plist`'s `BGTaskSchedulerPermittedIdentifiers`.
    public static let taskIdentifier = "me.lakko.dev.contact-refresh"

    /// Earliest the OS should consider running the next refresh. 15 minutes is
    /// the practical floor iOS honours for `BGAppRefreshTask`; a smaller value
    /// is silently clamped.
    private static let earliestInterval: TimeInterval = 15 * 60

    /// Submit a request for the next background inbox refresh.
    ///
    /// Best-effort: `submit` throws when the OS declines (too many pending
    /// requests, Low Power Mode, an unentitled simulator build). A failure is
    /// not a correctness gap — the foreground poll still covers the active
    /// session — so it is logged and swallowed.
    public static func schedule() {
        let request = BGAppRefreshTaskRequest(identifier: taskIdentifier)
        request.earliestBeginDate = Date(timeIntervalSinceNow: earliestInterval)
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            NSLog("[contacts] BGAppRefresh submit failed: \(error.localizedDescription)")
        }
    }
}
