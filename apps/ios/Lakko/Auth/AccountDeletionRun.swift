// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// One server-side cleanup that only this device can do after the account
/// is gone at auth (Settings > Account > Delete account).
///
/// `name` is a fixed label for logs; it never carries an identifier.
public struct AccountDeletionCleanup {
    public let name: String
    public let run: @MainActor () async throws -> Void

    public init(name: String, run: @escaping @MainActor () async throws -> Void) {
        self.name = name
        self.run = run
    }
}

/// The order of an account deletion, written once so it can be tested:
///
///   0. ``prepare``: what must be done while the account still exists at
///      auth (taking the `push-register` tokens the unsubscribes will spend;
///      auth issues them only to a live session). It cannot fail the run.
///   1. auth `DELETE /account`. If it throws (a refused proof, the ADR-0074
///      recall window, the network), the error propagates and NOTHING else
///      runs: no cleanup, no wipe.
///   2. Every cleanup, concurrently, each retried a few times with a short
///      pause, the whole step bounded by ``overallTimeout``. A cleanup that
///      fails is logged by its fixed name and the error's type only, and
///      never stops the next step.
///   3. The device wipe.
///
/// Cleanups are best-effort because the auth row is the erasure that
/// matters; they exist so nothing is left on a server that only this device
/// could remove (its push-gw subscriptions, which no server can name) or that
/// it can remove faster (the backup blob, the directory rows).
@MainActor
public struct AccountDeletionRun {
    public var prepare: @MainActor () async -> Void = {}
    public var deleteAtAuth: @MainActor () async throws -> Void
    public var cleanups: [AccountDeletionCleanup]
    public var wipe: @MainActor () -> Void
    /// Attempts per cleanup (chosen, not measured).
    public var attemptsPerCleanup: Int = 3
    /// Pause between two attempts of one cleanup (chosen, not measured).
    public var retryDelay: Duration = .milliseconds(500)
    /// Bound on the whole cleanup step (chosen, not measured).
    public var overallTimeout: Duration = .seconds(10)
    /// Logs a line; never given an identifier.
    public var log: @MainActor (String) -> Void = { NSLog("%@", $0) }

    public init(
        prepare: @escaping @MainActor () async -> Void = {},
        deleteAtAuth: @escaping @MainActor () async throws -> Void,
        cleanups: [AccountDeletionCleanup],
        wipe: @escaping @MainActor () -> Void
    ) {
        self.prepare = prepare
        self.deleteAtAuth = deleteAtAuth
        self.cleanups = cleanups
        self.wipe = wipe
    }

    /// Runs the three steps. Throws only the auth refusal, in which case
    /// neither the cleanups nor the wipe ran.
    public func run() async throws {
        await prepare()
        try await deleteAtAuth()
        await runCleanups()
        wipe()
    }

    private func runCleanups() async {
        let attempts = max(1, attemptsPerCleanup)
        let delay = retryDelay
        let timeout = overallTimeout
        let log = self.log
        await withTaskGroup(of: Bool.self) { group in
            for cleanup in cleanups {
                group.addTask { @MainActor in
                    for attempt in 1...attempts {
                        if Task.isCancelled { break }
                        do {
                            try await cleanup.run()
                            return true
                        } catch {
                            if Task.isCancelled || attempt == attempts {
                                log("[account] WARN deletion cleanup \(cleanup.name) failed: \(type(of: error))")
                                break
                            }
                            try? await Task.sleep(for: delay)
                        }
                    }
                    return true
                }
            }
            // The bound: when it fires first, the cleanups still running are
            // cancelled (URLSession requests stop on cancellation).
            group.addTask {
                try? await Task.sleep(for: timeout)
                return false
            }
            var remaining = cleanups.count
            while remaining > 0, let finished = await group.next() {
                if finished {
                    remaining -= 1
                } else {
                    log("[account] WARN deletion cleanups timed out")
                    break
                }
            }
            group.cancelAll()
        }
    }
}

/// Tokens taken before the session ends, handed out one by one.
///
/// `POST /push/unsubscribe` carries no session, but each one spends a
/// `push-register` anonymous token, and auth issues those only to a live
/// session. Once `DELETE /account` has removed the account, no token can be
/// issued, so the tokens the unsubscribes need are taken before it.
public final class ReservedAnonTokens: AnonTokenSource, @unchecked Sendable {
    private let lock = NSLock()
    private var tokens: [AnonToken]

    public init(_ tokens: [AnonToken]) {
        self.tokens = tokens
    }

    public var count: Int { lock.withLock { tokens.count } }

    /// How many tokens a deletion takes for `subscriptions` unsubscribes:
    /// one each, plus two for a token refusal's retry, at most
    /// ``maxForDeletion`` (auth's daily `push-register` allowance).
    public static func countForDeletion(subscriptions: Int) -> Int {
        subscriptions == 0 ? 0 : min(subscriptions + 2, maxForDeletion)
    }

    public static let maxForDeletion = 400

    /// Top up to at least `count` tokens of `context`: first from
    /// `takeAvailable` (the pool's stock), then issued by `issuer` with
    /// `sessionToken` while it still names a live account. Never throws: a
    /// shortfall is logged and only means fewer removals succeed.
    public func fill(
        _ context: AnonTokenContext,
        count: Int,
        takeAvailable: () -> AnonToken?,
        issuer: AnonTokenIssuing,
        sessionToken: String,
        now: Date = Date()
    ) async {
        var taken: [AnonToken] = []
        let held = lock.withLock { tokens.filter { $0.context == context.rawValue }.count }
        while held + taken.count < count, let token = takeAvailable() {
            taken.append(token)
        }
        let missing = count - held - taken.count
        if missing > 0 {
            let secs = UInt64(max(0, now.timeIntervalSince1970))
            let epoch = (try? anonTokenEpochAt(
                unixSecs: secs, windowSecs: anonTokenDefaultWindowSecs())) ?? 0
            let batch = UInt32(min(missing, Int(anonTokenMaxBatch())))
            do {
                taken += try await issuer.issue(
                    context: context, epoch: epoch, count: batch, sessionToken: sessionToken)
            } catch AnonTokenUnavailable.epochNotIssuable(let current) {
                taken += (try? await issuer.issue(
                    context: context, epoch: current, count: batch, sessionToken: sessionToken)) ?? []
            } catch {
                NSLog("[account] WARN push tokens for deletion not issued: \(type(of: error))")
            }
        }
        let kept = taken.filter { $0.context == context.rawValue }
        lock.withLock { tokens += kept }
    }

    public func take(_ context: AnonTokenContext) async throws -> AnonToken {
        let token: AnonToken? = lock.withLock {
            guard let index = tokens.firstIndex(where: { $0.context == context.rawValue }) else {
                return nil
            }
            return tokens.remove(at: index)
        }
        guard let token else { throw AnonTokenUnavailable.noSession }
        return token
    }
}
