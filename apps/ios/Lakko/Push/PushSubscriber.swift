// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import UIKit
import UserNotifications

/// APNs subscribe / unsubscribe orchestrator.
///
/// Mirror of:
///
///   * `apps/web/src/push/subscribe.ts` (1.20b)
///   * `apps/android/.../push/PushSubscriber.kt` (1.20d)
///
/// Joins three layers:
///
///   1. **`UNUserNotificationCenter.requestAuthorization`** — the
///      user-facing permission prompt for alert / badge / sound.
///   2. **`UIApplication.registerForRemoteNotifications`** — the
///      OS-level handshake with APNs that ends with a `Data`
///      device token delivered to the
///      `UIApplicationDelegate` callback. The token is shuttled
///      back here via ``PushSubscriber/receive(deviceToken:)`` /
///      ``PushSubscriber/registrationFailed(error:)`` — see the
///      ``PushAppDelegate`` adaptor.
///   3. **`PushClient.register`** — POST the token bytes to
///      `lakko-svc-push-gw` so it knows where to fan out wakes.
///
/// ## Why an actor for the token hand-off
///
/// `UIApplication.registerForRemoteNotifications` returns
/// immediately; the actual token (or error) arrives later on the
/// main thread inside the `AppDelegate` callback. We bridge that
/// to async/await via a `CheckedContinuation` parked inside the
/// actor — at most one register call is in flight at a time, and
/// double-resume is rejected by the actor's guard.
///
/// ## Default base URL
///
/// ``PushClient/defaultBaseURL`` — derived from the build's API base URL.
@MainActor
public final class PushSubscriber {

    public static let shared = PushSubscriber()

    private let client: PushClient
    private let bindingStore: PushAccountBindingStore
    private var pendingContinuation: CheckedContinuation<Data, Error>?

    /// The newest ``unsubscribe(prepaidToken:)`` (each one runs after the one
    /// before it). A ``subscribe(accountIdB64:stillSignedIn:)`` waits for it
    /// first: the device pseudonym is one per device, so a sign-out's
    /// unregister that is still in flight when the next account signs in
    /// would otherwise take that account's registration off the server and
    /// clear the pseudonym it has just been bound under.
    private var unsubscribing: Task<Void, Never>?

    public init(
        client: PushClient = PushClient(),
        bindingStore: PushAccountBindingStore = .live
    ) {
        self.client = client
        self.bindingStore = bindingStore
    }

    // MARK: - Subscribe

    /// Request authorization, register with APNs, capture the
    /// device token, and tell `push-gw` about it. Idempotent —
    /// iOS caches the APNs token internally; a fresh
    /// `registerForRemoteNotifications` call re-delivers the same
    /// bytes through the AppDelegate callback unless the user has
    /// reset the token (e.g. backup restore).
    ///
    /// Returns ``SubscribeResult`` on success.
    /// Throws ``PushSubscribeError`` for the documented no-go
    /// conditions (permission denied, APNs error, push-gw
    /// register error). Callers treat any error as "push not
    /// available, fall back to foreground polling"; auth flows
    /// should `try?` this so a failure doesn't block sign-in.
    ///
    /// ADR-0042 (amended 2026-09-30) — the pseudonym is registered FOR
    /// `accountIdB64`, and that pairing is recorded
    /// (``PushAccountBindingStore``) before the token reaches push-gw, so the
    /// first wake already finds it. `stillSignedIn` is asked immediately
    /// before the write, after the awaits above it: a Log out that ran while
    /// this subscribe was waiting for its APNs token has already cleared the
    /// binding, and a late subscribe must not put it back.
    public func subscribe(
        accountIdB64: String,
        stillSignedIn: @MainActor () -> Bool
    ) async throws -> SubscribeResult {
        // A sign-out's unregister still in flight finishes first.
        await unsubscribing?.value
        // (1) Authorization prompt. If the user has previously
        // denied, the system returns `false` without showing
        // the prompt again — we surface this as
        // ``PushSubscribeError/permissionDenied``.
        let center = UNUserNotificationCenter.current()
        let granted = try await center.requestAuthorization(
            options: [.alert, .badge, .sound]
        )
        guard granted else {
            throw PushSubscribeError.permissionDenied
        }

        // (2) Register with APNs + wait for the token.
        let token = try await registerAndAwaitToken()

        // (3) Persist the device pseudonym + tell push-gw.
        let pseudonym: Data
        do {
            pseudonym = try DevicePushId.getOrCreate()
        } catch {
            throw PushSubscribeError.deviceIdUnavailable(error)
        }
        guard stillSignedIn() else {
            throw PushSubscribeError.signedOut
        }
        bindingStore.bind(devicePseudonym: pseudonym, accountIdB64: accountIdB64)

        do {
            try await client.register(
                devicePseudonym: pseudonym,
                provider: .apns,
                tokenBytes: token
            )
        } catch {
            throw PushSubscribeError.registerFailed(error)
        }
        // `docs/ux/calls.md` section 2.1 — the VoIP token is registered under
        // the same pseudonym. PushKit delivers it once per launch, so a
        // sign-in in a running app registers it here. Best effort inside.
        await VoipPushRegistrar.shared.registerForSignIn(devicePseudonym: pseudonym)

        return SubscribeResult(
            devicePseudonym: pseudonym,
            apnsToken: token
        )
    }

    /// Drop the push registration locally + tell `push-gw` to
    /// forget the token. Called on logout + forgetDevice.
    /// Idempotent + best-effort: failures are logged.
    ///
    /// `prepaidToken` (ADR-0054 S3): the `push-register` token the
    /// unregister spends, taken by the caller before it cleared the session;
    /// without one the unregister draws from the pool.
    public func unsubscribe(prepaidToken: AnonToken? = nil) async {
        let previous = unsubscribing
        let task = Task { @MainActor [weak self] in
            await previous?.value
            await self?.performUnsubscribe(prepaidToken: prepaidToken)
        }
        unsubscribing = task
        await task.value
    }

    private func performUnsubscribe(prepaidToken: AnonToken?) async {
        let pseudonym: Data?
        do {
            pseudonym = try DevicePushId.peek()
        } catch {
            pseudonym = nil
        }
        if let pseudonym {
            do {
                _ = try await client.unregister(
                    devicePseudonym: pseudonym,
                    provider: .apns,
                    prepaidToken: prepaidToken
                )
            } catch {
                // Server-side unregister failed — log and
                // proceed. The local cleanup is the user-
                // visible "I'm signing out now" intent.
                NSLog("push unregister failed: \(error)")
            }
        }
        // `docs/ux/calls.md` section 2.1 — the VoIP token is a SECOND
        // registration under its own provider (``PushTokenKind``), so signing
        // out has to drop it too; the alert unregister above does not touch it.
        // Under the pseudonym read above, before the await: this sign-in's.
        if let pseudonym {
            await VoipPushRegistrar.shared.unregister(devicePseudonym: pseudonym)
        }
        // iOS doesn't expose a `unregisterForRemoteNotifications`
        // in 16+ (it was deprecated in 9 and removed). Apple's
        // canonical advice: just stop registering. The OS keeps
        // the APNs token alive until the app is uninstalled —
        // we drop our server-side row, the OS-side state is
        // harmless without the corresponding push-gw record.
        DevicePushId.clear()
    }

    // MARK: - AppDelegate callback hand-off

    /// Called by the ``PushAppDelegate`` adaptor when the OS
    /// delivers the APNs device token. Resumes the parked
    /// continuation from ``subscribe()``.
    public func receive(deviceToken: Data) {
        guard let cont = pendingContinuation else {
            // No subscribe in flight — token arrived through
            // some other code path (e.g. a system-driven re-
            // register on token refresh). Stash for the next
            // subscribe call? For 1.20c we ignore — the next
            // subscribe will trigger a fresh registration and
            // pick up the same token.
            return
        }
        pendingContinuation = nil
        cont.resume(returning: deviceToken)
    }

    /// Called by the ``PushAppDelegate`` adaptor when APNs
    /// registration fails. Resumes the parked continuation with
    /// the error.
    public func registrationFailed(error: Error) {
        guard let cont = pendingContinuation else {
            return
        }
        pendingContinuation = nil
        cont.resume(throwing: PushSubscribeError.apns(error))
    }

    // MARK: - Internals

    private func registerAndAwaitToken() async throws -> Data {
        // Reject a second concurrent subscribe — the token
        // delivery is process-singleton and there's no way to
        // disambiguate two parked continuations from the
        // AppDelegate callback.
        if pendingContinuation != nil {
            throw PushSubscribeError.concurrentSubscribe
        }
        return try await withCheckedThrowingContinuation { cont in
            pendingContinuation = cont
            UIApplication.shared.registerForRemoteNotifications()
        }
    }
}

// MARK: - Result + errors

public struct SubscribeResult: Equatable {
    public let devicePseudonym: Data
    public let apnsToken: Data

    public init(devicePseudonym: Data, apnsToken: Data) {
        self.devicePseudonym = devicePseudonym
        self.apnsToken = apnsToken
    }
}

public enum PushSubscribeError: Error {
    /// User declined the `UNUserNotificationCenter` prompt (or
    /// had declined previously and the system suppressed the
    /// re-prompt).
    case permissionDenied
    /// APNs registration failed (no network, expired entitlement,
    /// etc).
    case apns(Error)
    /// `DevicePushId.getOrCreate()` failed — Keychain
    /// unreachable.
    case deviceIdUnavailable(Error)
    /// `PushClient.register` rejected the upload (server
    /// failure / no anonymous token / network).
    case registerFailed(Error)
    /// Two ``PushSubscriber/subscribe(accountIdB64:stillSignedIn:)``
    /// calls overlapped. Only one register can be in flight at a time —
    /// the OS callback can't distinguish two parked continuations.
    case concurrentSubscribe
    /// The account signed out while the subscribe was waiting; nothing was
    /// bound or registered.
    case signedOut
}

extension PushSubscribeError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .permissionDenied:
            return "Notification permission denied"
        case .apns(let e):
            return "APNs registration failed: \(e)"
        case .deviceIdUnavailable(let e):
            return "Device push id unavailable: \(e)"
        case .registerFailed(let e):
            return "Push gateway register failed: \(e)"
        case .concurrentSubscribe:
            return "Another push subscribe is already in flight"
        case .signedOut:
            return "Signed out before the push subscribe finished"
        }
    }
}
