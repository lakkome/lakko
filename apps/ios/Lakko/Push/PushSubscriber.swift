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
///   3. **The sealed subscriptions** (ADR-0078 section 3) — the token is
///      kept on the device (``PushDeviceTokens``) and sealed into one push-gw
///      row per group (``PushGroupRouting/resubscribeAll(accountIdB64:)``);
///      push-gw holds no device pseudonym and no token in the clear.
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
    private let tokens: PushDeviceTokens
    private var pendingContinuation: CheckedContinuation<Data, Error>?

    /// The routing the sealed subscriptions are sent through; set by the
    /// composition root (``ChatRepository`` owns it). `nil` sends nothing.
    public var routing: PushGroupRouting?

    /// The newest ``unsubscribe(accountIdB64:prepaidToken:)`` (each one runs
    /// after the one before it). A ``subscribe(accountIdB64:stillSignedIn:)``
    /// waits for it first, so a sign-out's removals still in flight cannot
    /// interleave with the next sign-in's subscriptions.
    private var unsubscribing: Task<Void, Never>?

    public init(
        client: PushClient = PushClient(),
        bindingStore: PushAccountBindingStore = .live,
        tokens: PushDeviceTokens = .shared
    ) {
        self.client = client
        self.bindingStore = bindingStore
        self.tokens = tokens
    }

    // MARK: - Subscribe

    /// Request authorization, register with APNs, capture the device token,
    /// and send every subscription of `accountIdB64` sealed with it
    /// (ADR-0078). Idempotent — iOS caches the APNs token internally.
    ///
    /// Throws ``PushSubscribeError`` for the documented no-go conditions.
    /// Callers treat any error as "push not available, fall back to
    /// foreground polling"; auth flows `try?` this so a failure doesn't block
    /// sign-in.
    ///
    /// ADR-0042 (amended 2026-09-30) — the account is recorded as the bound
    /// one (``PushAccountBindingStore``) before any subscription reaches
    /// push-gw, so the first wake already finds it. `stillSignedIn` is asked
    /// immediately before the write, after the awaits above it: a Log out that
    /// ran while this subscribe was waiting for its APNs token has already
    /// cleared the binding, and a late subscribe must not put it back. When
    /// the binding named ANOTHER account (it never logged out), that account's
    /// subscriptions are removed first.
    public func subscribe(
        accountIdB64: String,
        stillSignedIn: @MainActor () -> Bool
    ) async throws -> SubscribeResult {
        // A sign-out's removals still in flight finish first.
        await unsubscribing?.value
        // (1) Authorization prompt.
        let center = UNUserNotificationCenter.current()
        let granted = try await center.requestAuthorization(
            options: [.alert, .badge, .sound]
        )
        guard granted else {
            throw PushSubscribeError.permissionDenied
        }

        // (2) Register with APNs + wait for the token.
        let token = try await registerAndAwaitToken()

        guard stillSignedIn() else {
            throw PushSubscribeError.signedOut
        }
        if let previous = bindingStore.current()?.accountIdB64, previous != accountIdB64 {
            await retire(subscriptionIds: PushRoutingStore().subscriptionIds(accountIdB64: previous))
        }
        tokens.setAlert(token)
        bindingStore.bind(accountIdB64: accountIdB64)
        // `docs/ux/calls.md` section 2.1 — the VoIP token rides the same
        // subscriptions; PushKit delivers it once per launch.
        VoipPushRegistrar.shared.noteCurrentToken()
        // (3) Every subscription carries the tokens sealed: send each.
        await routing?.resubscribeAll(accountIdB64: accountIdB64)
        return SubscribeResult(apnsToken: token)
    }

    /// The tokens changed while `accountIdB64` stayed signed in (a new VoIP
    /// token, an invalidated one): send every subscription again.
    public func tokensChanged() async {
        guard let account = bindingStore.current()?.accountIdB64 else { return }
        await routing?.resubscribeAll(accountIdB64: account)
    }

    /// Remove `accountIdB64`'s sealed subscriptions at push-gw and forget
    /// this device's tokens. Called on logout + forgetDevice. Idempotent +
    /// best-effort: failures are logged.
    ///
    /// The subscription ids live in the account's route rows; a wipe that ran
    /// first hands the ids it removed to ``PendingPushUnsubscribes``, which
    /// this drains.
    ///
    /// `prepaidToken` (ADR-0054 S3): one `push-register` token taken by the
    /// caller before it cleared the session, for the first removal; the rest
    /// draw from the pool.
    public func unsubscribe(accountIdB64: String?, prepaidToken: AnonToken? = nil) async {
        let previous = unsubscribing
        let task = Task { @MainActor [weak self] in
            await previous?.value
            await self?.performUnsubscribe(accountIdB64: accountIdB64, prepaidToken: prepaidToken)
        }
        unsubscribing = task
        await task.value
    }

    private func performUnsubscribe(accountIdB64: String?, prepaidToken: AnonToken?) async {
        var ids = PendingPushUnsubscribes.drain()
        if let accountIdB64 {
            ids.append(contentsOf: PushRoutingStore().subscriptionIds(accountIdB64: accountIdB64))
        }
        await retire(subscriptionIds: ids, prepaidToken: prepaidToken)
        // iOS doesn't expose a `unregisterForRemoteNotifications` in 16+;
        // Apple's advice: just stop registering. The OS keeps the APNs token
        // alive until the app is uninstalled; with no push-gw row it is
        // harmless.
        tokens.clear()
    }

    /// `POST /push/unsubscribe` for each id, one after another, best-effort.
    private func retire(subscriptionIds: [Data], prepaidToken: AnonToken? = nil) async {
        var prepaid = prepaidToken
        var seen: Set<Data> = []
        for subId in subscriptionIds where seen.insert(subId).inserted {
            do {
                try await client.unsubscribe(subId: subId, prepaidToken: prepaid)
            } catch {
                // A row left behind holds tokens sealed under a key push-gw
                // only sees inside a wake; it goes when the provider reports
                // the token invalid.
                NSLog("push unsubscribe failed: \(error)")
            }
            prepaid = nil
        }
    }

    // MARK: - AppDelegate callback hand-off

    /// Called by the ``PushAppDelegate`` adaptor when the OS
    /// delivers the APNs device token. Resumes the parked
    /// continuation from ``subscribe(accountIdB64:stillSignedIn:)``.
    public func receive(deviceToken: Data) {
        guard let cont = pendingContinuation else {
            // No subscribe in flight — a system-driven re-register. A token
            // that CHANGED while an account is bound must reach every
            // subscription (ADR-0078: each one carries it sealed).
            if bindingStore.current() != nil, tokens.setAlert(deviceToken) {
                Task { @MainActor [weak self] in await self?.tokensChanged() }
            }
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
    public let apnsToken: Data

    public init(apnsToken: Data) {
        self.apnsToken = apnsToken
    }
}

/// Subscription ids a wipe removed from the route rows before the sign-out's
/// unsubscribe could read them (ADR-0078): ``PushSubscriber`` drains them.
/// Foundation-only, in the app's own `UserDefaults`.
public enum PendingPushUnsubscribes {
    private static let key = "me.lakko.push-sub.pending-unsubscribe.v1"

    /// Remember `ids` for the next unsubscribe.
    public static func stash(_ ids: [Data], defaults: UserDefaults = .standard) {
        guard !ids.isEmpty else { return }
        let existing = defaults.stringArray(forKey: key) ?? []
        defaults.set(existing + ids.map { PushAccountBinding.hex($0) }, forKey: key)
    }

    /// Take every stashed id.
    public static func drain(defaults: UserDefaults = .standard) -> [Data] {
        let hexes = defaults.stringArray(forKey: key) ?? []
        defaults.removeObject(forKey: key)
        return hexes.compactMap { hex in
            guard let normal = PushAccountBinding.normalizedSubscriptionIdHex(hex) else { return nil }
            var out = Data(capacity: 16)
            var index = normal.startIndex
            while index < normal.endIndex {
                let next = normal.index(index, offsetBy: 2)
                out.append(UInt8(normal[index..<next], radix: 16) ?? 0)
                index = next
            }
            return out
        }
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
        case .concurrentSubscribe:
            return "Another push subscribe is already in flight"
        case .signedOut:
            return "Signed out before the push subscribe finished"
        }
    }
}
