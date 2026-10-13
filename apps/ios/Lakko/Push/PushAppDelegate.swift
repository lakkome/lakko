// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import UIKit
import UserNotifications

/// `UIApplicationDelegate` adaptor that bridges the two APNs
/// registration callbacks into the actor-isolated
/// ``PushSubscriber/receive(deviceToken:)`` and
/// ``PushSubscriber/registrationFailed(error:)`` hand-offs.
///
/// Wired into the SwiftUI app via
/// `@UIApplicationDelegateAdaptor(PushAppDelegate.self)` on
/// ``LakkoApp``.
///
/// ## Why an AppDelegate at all
///
/// SwiftUI's `App` lifecycle does not surface
/// `application(_:didRegister*…)`. iOS routes APNs
/// registration results to the legacy UIKit `UIApplicationDelegate`
/// regardless of whether the app is SwiftUI-only. The standard
/// Apple-blessed bridge is a small `NSObject` adaptor like this
/// one, hooked via `@UIApplicationDelegateAdaptor`.
///
/// The adaptor is deliberately tiny — every method just forwards
/// to the singleton ``PushSubscriber`` so the real logic lives in
/// the actor-isolated subscriber, not in a delegate class that's
/// hard to test in isolation.
public final class PushAppDelegate: NSObject, UIApplicationDelegate,
    UNUserNotificationCenterDelegate
{

    public func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [
            UIApplication.LaunchOptionsKey: Any
        ]? = nil
    ) -> Bool {
        // 1.20c — claim the foreground-presentation callback so
        // pushes received while the app is open show as in-app
        // banners (not silently dropped). The actual decision
        // about how to display lives in
        // `userNotificationCenter(_:willPresent:withCompletionHandler:)`.
        UNUserNotificationCenter.current().delegate = self
        // `docs/ux/calls.md` section 2.1 — start receiving VoIP pushes at
        // launch, not at sign-in: the token is device state, and the app has to
        // be able to take a call push before it knows whether one is coming.
        // iOS delivers the current token to the registrar's delegate on
        // every launch; it is registered then for a restored session, and at
        // sign-in otherwise (``VoipPushRegistrar/registerForSignIn(devicePseudonym:)``).
        VoipPushRegistrar.shared.start()
        return true
    }

    // MARK: - APNs registration callbacks

    public func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { @MainActor in
            PushSubscriber.shared.receive(deviceToken: deviceToken)
        }
    }

    public func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        Task { @MainActor in
            PushSubscriber.shared.registrationFailed(error: error)
        }
    }

    // MARK: - Foreground notification presentation

    /// Called when a remote notification arrives while the app
    /// is in the foreground. Default OS behaviour is to drop
    /// the alert (assuming the in-app UI already shows the
    /// content); we surface a banner + sound so a user who's
    /// not currently looking at the relevant chat still sees
    /// the wake.
    public func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (
            UNNotificationPresentationOptions
        ) -> Void
    ) {
        // `docs/ux/calls.md` section 2.1, first row — with the app open, an
        // incoming call takes over Lakko's own screen, not the notification
        // shade. The extension's call alert is the fallback for a device whose
        // app is NOT running; presenting it over a running app would double up
        // with the in-app incoming-call surface that is already showing.
        let content = notification.request.content
        if content.categoryIdentifier == LakkoCallNotification.callCategoryIdentifier {
            completionHandler([])
            return
        }
        // Everything else: the decrypted message preview the NSE wrote
        // (ADR-0041), or the generic fallback when it could not.
        completionHandler([.banner, .sound, .badge])
    }

    /// Called when the user taps a notification. The wake hint
    /// (group pseudonym, envelope pointer) lives in the
    /// notification's `userInfo` payload — we hand it to the
    /// global ``InviteIntake`` analog (a future
    /// `PushIntake`) so the SwiftUI router can deep-link to the
    /// relevant chat. For 1.20c we just bring the app to the
    /// foreground; the user re-finds the unread thread via the
    /// chat list.
    public func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        // 1.20c — no deep-link routing yet. The
        // `notification.request.content.userInfo` carries the
        // wake hint; route layer wiring is a 1.20c2 follow-up
        // when the chat-list deep-link path materialises.
        completionHandler()
    }
}
