// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import UserNotifications

/// `UNNotificationServiceExtension` for the Lakko iOS app
/// (Phase 1.20c2).
///
/// iOS invokes this extension before displaying any APNs alert
/// push that carries `aps.mutable-content = 1`. The extension
/// has ~30 seconds + 30 MB of memory to mutate the
/// notification's content before iOS shows it on the lock
/// screen. We use the budget for one thing in 1.20c2:
///
///   * Set `threadIdentifier` to the wake payload's group hex
///     (`g` field) so iOS groups notifications per conversation
///     on the lock screen instead of piling them up under one
///     "Lakko" bucket.
///
/// **What 1.20c2 deliberately does NOT do**: decrypt the MLS
/// application message to render "Alice: hey" preview. That
/// requires the full `lakko-mls` provider state + canonical
/// Identity, both of which currently live in the main app's
/// Keychain + IDB-analog file container. Bridging them to the
/// extension needs:
///
///   * An App Group shared container (provisioned in
///     `apps/ios/project.yml`'s entitlements — sets the
///     ground for the follow-up; the entitlement is in place
///     but we don't read it yet).
///   * A slim `lakko-ffi-extension` xcframework — the full
///     `LakkoCore.xcframework` (~50 MB linked) blows past the
///     extension's memory budget. The full openmls + provider
///     surface needs to be carved into a smaller decrypt-only
///     subset.
///   * Migration of the MLS provider's persistence backend
///     from per-bundle Keychain to App-Group-shared file
///     storage (mirror writes from main app → extension reads).
///
/// All three are weeks of work. They land as a 1.20c2b follow-
/// up. In the meantime, the lock-screen grouping the
/// `threadIdentifier` flip gives us is real user value: a chat
/// that gets 5 messages in a minute shows as one stack instead
/// of five separate banners.
///
/// ## Fallback posture
///
/// Every failure path (missing payload field, exceeded
/// budget, exception in our code) MUST call `contentHandler`
/// with the unmodified `bestAttemptContent`. iOS shows the
/// original "Lakko / New message" alert in that case — same
/// fallback the main app foreground delegate uses. Skipping
/// `contentHandler` would silently drop the notification,
/// which is far worse than a generic preview.
public final class NotificationService: UNNotificationServiceExtension {

    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var bestAttemptContent: UNMutableNotificationContent?

    public override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        self.contentHandler = contentHandler
        self.bestAttemptContent =
            request.content.mutableCopy() as? UNMutableNotificationContent

        guard let best = bestAttemptContent else {
            // Shouldn't be reachable — UNMutableNotificationContent
            // is a documented superset of the immutable original
            // — but defensive: deliver the original unchanged.
            contentHandler(request.content)
            return
        }

        // Mutate: set threadIdentifier from the wake payload's
        // `g` (group_pseudonym hex) field so iOS groups
        // notifications per conversation. The APNs provider in
        // `services/push-gw/src/providers/apns.rs` (1.20c2)
        // also pre-populates `aps.thread-id` with the same
        // value, but the extension is allowed to override and
        // we do it explicitly here to keep the mutation logic
        // visible.
        if let groupHex = request.content.userInfo["g"] as? String,
           !groupHex.isEmpty {
            best.threadIdentifier = groupHex
            // Subtitle: short prefix of the group hex so the
            // user can tell two simultaneous conversations
            // apart visually. iOS shows subtitle below the
            // title on the lock screen. A real conversation
            // name needs the App Group MLS state — 1.20c2b.
            best.subtitle = "in \(groupHex.prefix(8))"
        }

        contentHandler(best)
    }

    public override func serviceExtensionTimeWillExpire() {
        // ~5 seconds left on the 30s budget. iOS will kill us
        // if we don't call `contentHandler` — deliver
        // `bestAttemptContent` as a best-effort fallback. Same
        // posture as the canonical Apple sample code.
        if let contentHandler = contentHandler,
           let bestAttemptContent = bestAttemptContent {
            contentHandler(bestAttemptContent)
        }
    }
}
