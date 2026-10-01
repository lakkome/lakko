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
/// ## Only the signed-in account (ADR-0042, amended 2026-09-30)
///
/// The decrypt runs only when the wake's `d` equals the device pseudonym the
/// app bound to the signed-in account (``PushAccountBinding``), and only as
/// that account. A wake for no account (signed out) or for another sign-in
/// session gets the generic line below with no content: this extension must
/// display something for a mutable-content alert (it holds no filtering
/// entitlement), so the generic line is the most it may show.
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
        // The notification words in the interface language the app stored
        // (ADR-0061); English per word when it has not.
        nseNotificationWordLookup = { SharedStorage.storedNotificationWord($0) }

        guard let best = bestAttemptContent else {
            // Shouldn't be reachable — UNMutableNotificationContent is a
            // documented superset of the immutable original — but defensive:
            // deliver the original unchanged.
            contentHandler(request.content)
            return
        }
        // Every path that delivers without a decrypted preview shows the
        // server's generic placeholder: in the interface language, not the
        // server's English (ADR-0061).
        best.body = nseGenericBody(best.body)

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
            let outcome = await decryptPushOutcome(wake, NseRealDecryptDeps())
            guard let self else { return }
            switch outcome {
            case .preview(let preview):
                best.body = preview.body
                self.deliver(best)
            case .callOffer(let alert):
                self.deliverCallAlert(alert, into: best)
            case .callSilent(let alert):
                self.deliverSilentCallAlert(alert, into: best)
            case .callEnded(let callIdHex):
                await self.deliverCallEnded(callIdHex: callIdHex, into: best)
            case .none:
                self.deliver(best)
            }
        }
    }

    // MARK: - Calls

    /// A call is ringing and no VoIP push arrived (`docs/ux/calls.md` section
    /// 2.1).
    ///
    /// This is the fallback, and it is honest about being one: only CallKit can
    /// make a phone ring like a phone, only the app can reach CallKit, and a
    /// notification extension can neither launch the app nor report a call. So
    /// the best it can do is a call-shaped alert — the caller's name, what kind
    /// of call it is, a ringtone rather than a message chime, and a time
    /// sensitive level so it breaks through a Focus — which the user taps to
    /// open the app, where the call is still ringing if it is still ringing.
    ///
    /// The call id rides in `userInfo` so two other things can find this alert:
    /// the "call ended" arm below, and the app itself when the call is answered
    /// or ended there (``CallKitBridge``).
    private func deliverCallAlert(
        _ alert: NseCallAlert,
        into best: UNMutableNotificationContent
    ) {
        best.title = alert.title
        best.body = alert.body
        best.threadIdentifier = alert.tag
        best.categoryIdentifier = LakkoCallNotification.callCategoryIdentifier
        // A ringtone, not the message chime: this is a call.
        best.sound = .defaultRingtone
        // Breaks through Focus / Do Not Disturb when the user has allowed
        // Lakko to.
        //
        // A muted conversation never reaches this method. It used to — this
        // comment claimed "a muted chat is not woken at all", which no code
        // anywhere supported: the gateway wakes every device the same way, and
        // nothing on this path had ever read the mute flag. A muted chat rang
        // a closed phone at full volume for as long as that sentence stood.
        // The flag is now read (``NsePushDecryptDeps/isMuted(accountIdB64:groupIdHex:)``,
        // through the App Group mirror) and a muted call lands in
        // ``deliverSilentCallAlert(_:into:)`` instead.
        best.interruptionLevel = .timeSensitive
        var info = best.userInfo
        info[LakkoCallNotification.callIdUserInfoKey] = alert.callIdHex
        best.userInfo = info
        deliver(best)
    }

    /// A call that must not interrupt: the conversation is muted, or the
    /// caller gave up before this wake was decrypted
    /// (`docs/ux/calls.md` section 2.1).
    ///
    /// ## Why anything is shown at all
    ///
    /// Because showing nothing is not available and would be wrong if it were.
    /// The extension MUST call `contentHandler`, so some notification is going
    /// to be delivered; and section 2.1 withholds the INTERRUPTION, not the
    /// record. On this platform the record is the only thing this process can
    /// leave behind at all — it may not write (C-59), so it cannot file the
    /// call-history row the app files on its own receive path, and a call that
    /// arrived while the app was closed could otherwise leave no trace
    /// anywhere.
    ///
    /// ## What makes it passive rather than a call
    ///
    /// No sound, and `.passive` — it does not light the screen, does not push
    /// past a Focus, and waits in the list. And deliberately NOT the call
    /// category and NOT the call-id key: those two are how the app's
    /// notification delegate recognises a RINGING call and how the "call
    /// ended" arm takes its alert back down. Wearing them, this would either
    /// offer to answer a call nobody is on, or erase itself the moment the
    /// call ended — which is exactly the record it exists to keep.
    private func deliverSilentCallAlert(
        _ alert: NseCallAlert,
        into best: UNMutableNotificationContent
    ) {
        best.title = alert.title
        best.body = alert.body
        best.threadIdentifier = alert.tag
        best.categoryIdentifier = ""
        best.sound = nil
        best.interruptionLevel = .passive
        deliver(best)
    }

    /// A call this device may be showing an alert for is over.
    ///
    /// Takes the alert down, and then says as little as it can: the extension
    /// MUST call `contentHandler` — skipping it drops the notification in a way
    /// iOS counts against the app — so "show nothing" is not available.
    /// ``nseEndedCallContent(hadDeliveredAlert:callerName:)`` decides what to
    /// say instead, and the answer turns on whether the offer alert was still
    /// on screen: still there means nobody dealt with the call here, which is a
    /// missed call (section 6); already gone means it was answered or declined
    /// here and there is nothing to report, so the notification is passive and
    /// neither lights the screen nor makes a sound.
    private func deliverCallEnded(
        callIdHex: String,
        into best: UNMutableNotificationContent
    ) async {
        let center = UNUserNotificationCenter.current()
        let delivered = await center.deliveredNotifications()
        let stale = delivered.filter {
            $0.request.content.userInfo[LakkoCallNotification.callIdUserInfoKey]
                as? String == callIdHex
        }
        if !stale.isEmpty {
            center.removeDeliveredNotifications(withIdentifiers: stale.map(\.request.identifier))
        }
        let content = nseEndedCallContent(
            hadDeliveredAlert: !stale.isEmpty,
            callerName: stale.first?.request.content.title
        )
        best.title = content.title
        best.body = content.body
        best.categoryIdentifier = LakkoCallNotification.callCategoryIdentifier
        best.sound = nil
        best.interruptionLevel = content.passive ? .passive : .active
        var info = best.userInfo
        info[LakkoCallNotification.callIdUserInfoKey] = callIdHex
        best.userInfo = info
        deliver(best)
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
    /// provider (`services/push-gw/src/providers/apns.rs`) puts the device
    /// pseudonym hex under `d`, the group pseudonym hex under `g` and the DS
    /// cursor under `p` (a JSON number).
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
        // `d` — the device pseudonym the wake was sent to. The decrypt core
        // handles the wake only when it equals the pseudonym the app bound to
        // the signed-in account (ADR-0042, amended 2026-09-30); a missing `d`
        // is carried as `nil` and refused there.
        let devicePseudonymHex = (userInfo["d"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        return NsePushWake(groupHex: groupHex, pointer: pointer, devicePseudonymHex: devicePseudonymHex)
    }
}
