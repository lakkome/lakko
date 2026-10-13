// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import UserNotifications

// ADR-0073 R.8 / R.7 case 1 - taking down what the extension could not
// withhold.
//
// A push whose rows the app shows nothing for (a superseded sender's, a
// re-send of a message already held) still produces a notification: the
// extension must deliver something, so it delivers it passively and marks it
// (`LakkoQuietNotification.userInfoKey`). The app, which may write, removes
// every delivered notification carrying the mark whenever it runs a
// continuity sweep, so the trace does not outlive the next time the app is
// open.
public enum QuietNotifications {

    /// The identifiers of the delivered notifications that carry the mark.
    public static func quietIdentifiers(
        _ delivered: [(identifier: String, userInfo: [AnyHashable: Any])]
    ) -> [String] {
        delivered
            .filter { ($0.userInfo[LakkoQuietNotification.userInfoKey] as? Bool) == true }
            .map(\.identifier)
    }

    /// Remove them. Fire-and-forget; nothing is reported.
    public static func removeDelivered() {
        Task {
            let center = UNUserNotificationCenter.current()
            let delivered = await center.deliveredNotifications().map {
                (identifier: $0.request.identifier, userInfo: $0.request.content.userInfo)
            }
            let ids = quietIdentifiers(delivered)
            guard !ids.isEmpty else { return }
            center.removeDeliveredNotifications(withIdentifiers: ids)
        }
    }
}
