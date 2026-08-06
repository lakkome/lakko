// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import UserNotifications

/// `UNNotificationServiceExtension` for the Lakko iOS app.
///
/// iOS invokes this extension before displaying any APNs alert push that
/// carries `aps.mutable-content = 1`. The extension has ~30 seconds + a tight
/// (~24-30 MB) memory budget to mutate the notification's content before iOS
/// shows it on the lock screen. It does two things:
///
///   * **Per-conversation grouping.** Sets `threadIdentifier` to the wake
///     payload's group hex (`g` field) so iOS groups notifications per
///     conversation on the lock screen. This is set FIRST and is retained
///     regardless of whether the decrypt below succeeds, so the grouping
///     works even when the extension is memory-pressured or the decrypt
///     fails.
///
///   * **Read-only throwaway MLS decrypt (ADR-0041 Phase 3).** For a
///     GroupMessage wake (`g` + `p` present), it reconstructs a FRESH
///     throwaway `Identity` from the shared AccountIdentity seed, restores a
///     TEMPORARY `Group` from the last-persisted MLS snapshot, does a
///     read-only DS fetch after the app's cursor, decrypts through the
///     audited path (`processIncomingVerifyingRoles` / `parseBodyEnvelope`),
///     and rewrites the notification body to the REAL newest-message preview
///     — then DISCARDS everything. It NEVER persists the advanced snapshot,
///     NEVER advances the app's cursor, NEVER writes any shared store (the
///     read-only-throwaway discipline `lakko-security` approved on web). The
///     foreground app later restores from the SAME snapshot and re-processes
///     from its OWN append-only cursor (idempotent), so the app's ratchet is
///     never advanced behind its back — the G-34-class desync hazard is
///     avoided by construction. Mirrors web (`sw-push-decrypt.js`) + Android
///     (`PushPreviewDecrypt.kt`) exactly.
///
/// ## Fallback posture (invariant #4)
///
/// Every failure path — a missing payload field, a non-member group, a DS or
/// decrypt failure, a frame that will not authenticate (dropped, never
/// guessed), the self-imposed budget deadline, or
/// `serviceExtensionTimeWillExpire` — delivers the unmodified generic
/// "New message" content via `contentHandler`. iOS shows the original alert
/// in that case — the same fallback the main app's foreground delegate uses.
/// Skipping `contentHandler` would silently DROP the notification, far worse
/// than a generic preview. Delivery is guarded so `contentHandler` is called
/// exactly once across the decrypt task, the budget deadline, and the
/// time-will-expire path.
public final class NotificationService: UNNotificationServiceExtension {

    /// Self-imposed wall-clock budget for the whole decrypt (DS fetch + MLS
    /// restore + decrypt), comfortably inside the ~30 s iOS hard limit. On
    /// timeout we deliver the generic content and let the (best-effort) task
    /// unwind on its own.
    private static let decryptBudgetSecs: TimeInterval = 22

    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var bestAttemptContent: UNMutableNotificationContent?

    /// Single-delivery guard — `contentHandler` must fire exactly once across
    /// the decrypt task completion, the budget deadline, and
    /// `serviceExtensionTimeWillExpire`.
    private let deliveryLock = NSLock()
    private var delivered = false

    /// The decrypt work + the budget deadline, so both can be cancelled on
    /// first delivery.
    private var decryptTask: Task<Void, Never>?
    private var budgetDeadline: DispatchWorkItem?

    public override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        self.contentHandler = contentHandler
        self.bestAttemptContent =
            request.content.mutableCopy() as? UNMutableNotificationContent

        guard let best = bestAttemptContent else {
            // Shouldn't be reachable — UNMutableNotificationContent is a
            // documented superset of the immutable original — but defensive:
            // deliver the original unchanged.
            contentHandler(request.content)
            return
        }

        // Grouping FIRST — retained regardless of the decrypt outcome so
        // per-conversation stacking works even under memory pressure or on a
        // decrypt failure. (The APNs provider also pre-populates
        // `aps.thread-id`; we override explicitly to keep the mutation
        // visible.)
        let userInfo = request.content.userInfo
        if let groupHex = userInfo["g"] as? String, !groupHex.isEmpty {
            best.threadIdentifier = groupHex
        }

        // Only a GroupMessage wake (a group hex + a DS pointer) is decryptable
        // in this slice. Anything else (a Wakeup / MailboxMessage wake) stays
        // generic.
        guard let wake = Self.parseWake(userInfo) else {
            deliver(best)
            return
        }

        // Budget deadline: if the decrypt has not delivered by the budget,
        // ship the generic content. Cancelled on first delivery.
        let deadline = DispatchWorkItem { [weak self] in
            self?.deliver(best)
        }
        budgetDeadline = deadline
        DispatchQueue.global(qos: .userInitiated)
            .asyncAfter(deadline: .now() + Self.decryptBudgetSecs, execute: deadline)

        // Run the read-only throwaway decrypt. Any failure returns nil (the
        // core never throws for a "cannot decrypt" reason; the whole call is
        // still guarded). On a real preview, rewrite the body; either way,
        // deliver exactly once.
        decryptTask = Task { [weak self] in
            let preview = await decryptPushPreview(wake, NseRealDecryptDeps())
            guard let self else { return }
            if let preview {
                best.body = preview.body
            }
            self.deliver(best)
        }
    }

    public override func serviceExtensionTimeWillExpire() {
        // ~5 seconds left on the iOS budget — iOS will kill us if we don't
        // call `contentHandler`. Deliver whatever we have (the generic
        // content if the decrypt has not finished). Same posture as the
        // canonical Apple sample code.
        if let best = bestAttemptContent {
            deliver(best)
        } else if let handler = contentHandler {
            handler(UNMutableNotificationContent())
        }
    }

    /// Deliver `content` via `contentHandler` at most once, cancelling the
    /// outstanding decrypt task + budget deadline so neither fires again.
    private func deliver(_ content: UNNotificationContent) {
        deliveryLock.lock()
        if delivered {
            deliveryLock.unlock()
            return
        }
        delivered = true
        let handler = contentHandler
        deliveryLock.unlock()

        budgetDeadline?.cancel()
        budgetDeadline = nil
        decryptTask?.cancel()
        decryptTask = nil
        handler?(content)
    }

    /// Decode a GroupMessage wake from the APNs `userInfo`. The push-gw APNs
    /// provider (`services/push-gw/src/providers/apns.rs`) puts the group
    /// pseudonym hex under `g` and the DS cursor under `p` (a JSON number).
    /// Returns `nil` when either is absent — the wake is not a decryptable
    /// GroupMessage.
    private static func parseWake(_ userInfo: [AnyHashable: Any]) -> NsePushWake? {
        guard let groupHex = userInfo["g"] as? String, !groupHex.isEmpty else {
            return nil
        }
        // `p` arrives as a JSON number → NSNumber; accept a numeric string
        // defensively too.
        let pointer: Int64?
        if let number = userInfo["p"] as? NSNumber {
            pointer = number.int64Value
        } else if let string = userInfo["p"] as? String, let value = Int64(string) {
            pointer = value
        } else {
            pointer = nil
        }
        guard let pointer else { return nil }
        return NsePushWake(groupHex: groupHex, pointer: pointer)
    }
}
