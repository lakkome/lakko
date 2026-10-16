// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The in-memory pool of anonymous tokens, per (account, context)
/// (ADR-0054 S3).
///
/// * Refilled on a randomised schedule that never looks at spending
///   (ADR-0078 section 8): when a session is bound, each context is topped
///   up once at a random moment within ``initialRefillSpread`` seconds, then
///   at independent random intervals between ``refillIntervalMin`` and
///   ``refillIntervalMax``, each time to its ``AnonTokenContext/poolTarget``
///   (about a day of use plus a reserve) in one issuance, never holding more
///   than ``maxHeld``. A take never starts an issuance: before ADR-0078 a
///   take below a low-water mark did, so the bearer issuance landed seconds
///   from the anonymous spend it refilled for.
/// * An action with no token at hand waits only for a refill the schedule
///   already runs (this session's first one, or one in flight), at most
///   ``waitSeconds``, then fails with the network copy until the next
///   scheduled refill.
/// * The issuance travels on the session-bearing `URLSession` and every spend
///   on ``AnonymousHTTP/session`` (its own ephemeral session), so the two
///   never share a connection.
/// * A token is live while `anonTokenIsLive(token.epoch, currentEpoch)`; the
///   current epoch is `anonTokenEpochAt(now, window)` corrected by the
///   issuer's `current_epoch` whenever it disagrees.
/// * Per account (maintainer rule, 2026-09-30): the in-memory tokens are the
///   bound account's only. An account switch drops them from memory and
///   loads the new account's own; logout drops them from memory only.
///   (``configure(accountKey:sessionToken:)``)
/// * Persisted per (account, context) between launches (ADR-0054 S5) through
///   an ``AnonTokenStore`` (the Keychain in production, this device only):
///   loaded when the pool is bound to an account, dead-epoch tokens dropped
///   on load, every change written back. Logout and an account switch keep
///   every account's stored tokens for that account's next sign-in; only
///   ``forget(accountKey:)`` (Forget device, Delete account, through
///   ``AccountLocalDataWipe``) deletes them, and only that account's.
///   Tokens are anonymous bearer credentials: never logged, never backed up,
///   never synced.
/// * A token drawn for one account is never handed to a request made under
///   another: every refill and every waiting ``take(_:)`` is tied to the
///   binding it started under, and is dropped or refused if the binding
///   changed meanwhile.
///
/// A lock-guarded class rather than an actor so ``takeIfAvailable(_:)`` and
/// ``configure(accountKey:sessionToken:)`` are synchronous: logout takes the
/// token its push unregister will spend BEFORE it clears the pool.
public final class AnonTokenPool: AnonTokenSource, @unchecked Sendable {

    /// The process-wide pool, fed by ``AuthRepository``.
    public static let shared = AnonTokenPool(
        issuer: AuthAnonTokenIssuer(), store: KeychainAnonTokenStore())

    /// Never hold more than this per context.
    public static let maxHeld = 32
    /// A context's first refill after a session is bound falls at a random
    /// moment within this many seconds.
    public static let initialRefillSpread: TimeInterval = 20
    /// After that, a context is topped up at independent random intervals
    /// between these (seconds). Chosen, not measured.
    public static let refillIntervalMin: TimeInterval = 30 * 60
    public static let refillIntervalMax: TimeInterval = 90 * 60
    /// How long an action waits for a refill before failing.
    public static let defaultWaitSeconds: TimeInterval = 8

    private var issuer: AnonTokenIssuing
    private let store: AnonTokenStore?
    private let now: @Sendable () -> Date
    private let windowSecs: UInt64
    private let waitSeconds: TimeInterval
    private let random: @Sendable () -> Double
    private let sleep: @Sendable (TimeInterval) async -> Void

    private let lock = NSLock()
    private var accountKey: String?
    /// Set by ``forget(accountKey:)`` for the bound account: nothing more is
    /// written to the store until the next binding.
    private var forgotten = false
    private var sessionToken: String?
    private var generation = 0
    private var stock: [AnonTokenContext: [AnonToken]] = [:]
    private var refills: [AnonTokenContext: Task<Void, Never>] = [:]
    /// The refill schedule of each context, for the bound session.
    private var schedules: [AnonTokenContext: Task<Void, Never>] = [:]
    /// The contexts whose first scheduled refill of this binding has not run
    /// yet: a take on an empty context waits for it.
    private var initialPending: Set<AnonTokenContext> = []
    private var lastFailure: [AnonTokenContext: AnonTokenUnavailable] = [:]
    private var quotaUntil: [AnonTokenContext: Date] = [:]
    /// The issuer's epoch minus this device's, learned from `epoch-not-issuable`.
    private var epochCorrection: Int64 = 0

    /// - Parameter store: where the tokens live between launches; `nil` keeps
    ///   them in memory only (tests).
    public init(
        issuer: AnonTokenIssuing,
        store: AnonTokenStore? = nil,
        now: @escaping @Sendable () -> Date = { Date() },
        windowSecs: UInt64 = anonTokenDefaultWindowSecs(),
        waitSeconds: TimeInterval = AnonTokenPool.defaultWaitSeconds,
        random: @escaping @Sendable () -> Double = { Double.random(in: 0..<1) },
        sleep: @escaping @Sendable (TimeInterval) async -> Void = { seconds in
            try? await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
        }
    ) {
        self.issuer = issuer
        self.store = store
        self.now = now
        self.windowSecs = windowSecs
        self.waitSeconds = waitSeconds
        self.random = random
        self.sleep = sleep
    }

    /// Replace the issuer refills are drawn from. A seam for the test bundle:
    /// ``shared`` is bound to the production issuer, and every client a test
    /// builds without naming a token source draws from it, so the bundle
    /// installs an issuer that never leaves the process before any test
    /// runs. A refill already in flight keeps the issuer it started with.
    func setIssuer(_ issuer: AnonTokenIssuing) {
        lock.lock()
        self.issuer = issuer
        lock.unlock()
    }

    // MARK: - Session

    /// Bind the pool to a session. A different `accountKey` (including `nil`,
    /// logout) drops every in-memory token and every refill in flight; the
    /// store keeps them for that account's next sign-in. The same account
    /// with a fresh session token keeps its tokens. Binding to an account
    /// loads what the store kept for it, minus the tokens whose epoch is no
    /// longer live; other accounts' stored tokens are left alone.
    public func configure(accountKey: String?, sessionToken: String?) {
        lock.lock()
        defer { lock.unlock() }
        let token = (sessionToken?.isEmpty ?? true) ? nil : sessionToken
        if accountKey != self.accountKey || accountKey == nil {
            generation += 1
            for task in refills.values { task.cancel() }
            refills = [:]
            for task in schedules.values { task.cancel() }
            schedules = [:]
            initialPending = []
            stock = [:]
            lastFailure = [:]
            quotaUntil = [:]
            epochCorrection = 0
            forgotten = false
            self.accountKey = accountKey
            if let store, let accountKey {
                stock = store.load(accountKey: accountKey)
                for context in AnonTokenContext.allCases {
                    let before = stock[context]?.count ?? 0
                    pruneLocked(context)
                    if (stock[context]?.count ?? 0) != before { persistLocked(context) }
                }
            }
        }
        self.sessionToken = accountKey == nil ? nil : token
        if self.sessionToken != nil, schedules.isEmpty, !forgotten {
            startScheduleLocked()
        }
    }

    // MARK: - The refill schedule (ADR-0078 section 8)

    /// Arm every context's schedule for the bound session: a first refill at
    /// a random moment within ``initialRefillSpread``, then one at a random
    /// interval between ``refillIntervalMin`` and ``refillIntervalMax``,
    /// for as long as the binding lasts. Called with the lock held.
    private func startScheduleLocked() {
        let generation = self.generation
        initialPending = Set(AnonTokenContext.allCases)
        for context in AnonTokenContext.allCases {
            let first = random() * Self.initialRefillSpread
            schedules[context] = Task { [weak self] in
                var delay = first
                while !Task.isCancelled {
                    guard let self else { return }
                    await self.sleep(delay)
                    if Task.isCancelled { return }
                    await self.scheduledRefill(context, generation: generation)
                    let span = Self.refillIntervalMax - Self.refillIntervalMin
                    delay = Self.refillIntervalMin + self.random() * span
                }
            }
        }
    }

    /// One scheduled top-up of `context` to its target, then the end of its
    /// first-refill wait.
    private func scheduledRefill(_ context: AnonTokenContext, generation: Int) async {
        let task: Task<Void, Never>? = lock.withLock {
            guard generation == self.generation else { return nil }
            pruneLocked(context)
            if (stock[context]?.count ?? 0) >= context.poolTarget { return nil }
            return try? startRefillLocked(context)
        }
        if let task { await task.value }
        lock.withLock {
            if generation == self.generation { initialPending.remove(context) }
        }
    }

    /// Run a scheduled top-up of every context now (what the schedule does
    /// one context at a time; tests and diagnostics).
    public func runScheduledRefills() async {
        let generation = lock.withLock { self.generation }
        for context in AnonTokenContext.allCases {
            await scheduledRefill(context, generation: generation)
        }
    }

    /// Unbind (logout): drop the in-memory tokens. The stored ones stay for
    /// the account's next sign-in.
    public func clear() {
        configure(accountKey: nil, sessionToken: nil)
    }

    /// Forget device / Delete account: delete every token stored for
    /// `accountKey`, and no other account's.
    ///
    /// When the pool is bound to `accountKey`, its in-memory tokens stay until
    /// the session is unbound (the push unregister that ends the session
    /// spends one), but nothing is written to the store for that binding
    /// again, so the deleted rows never come back.
    public func forget(accountKey: String) {
        lock.lock()
        defer { lock.unlock() }
        if self.accountKey == accountKey {
            forgotten = true
            for task in refills.values { task.cancel() }
            refills = [:]
            for task in schedules.values { task.cancel() }
            schedules = [:]
            initialPending = []
            generation += 1
        }
        store?.delete(accountKey: accountKey)
    }

    /// How many live tokens `context` holds (tests, diagnostics).
    public func count(_ context: AnonTokenContext) -> Int {
        lock.lock()
        defer { lock.unlock() }
        pruneLocked(context)
        return stock[context]?.count ?? 0
    }

    // MARK: - Taking

    /// A token already in the pool, or `nil`, without waiting and without
    /// starting a refill.
    public func takeIfAvailable(_ context: AnonTokenContext) -> AnonToken? {
        lock.lock()
        defer { lock.unlock() }
        return popLocked(context)
    }

    public func take(_ context: AnonTokenContext) async throws -> AnonToken {
        let binding = lock.withLock { generation }
        if let token = pop(context, binding: binding) { return token }
        // ADR-0078 section 8: a take never starts an issuance. It may wait for
        // one the schedule already runs.
        let pending: Task<Void, Never>? = try lock.withLock {
            guard sessionToken != nil else { throw AnonTokenUnavailable.noSession }
            if let until = quotaUntil[context], until.timeIntervalSince(now()) > 0 {
                throw AnonTokenUnavailable.quotaExhausted(
                    retryAfterSecs: Int(until.timeIntervalSince(now()).rounded(.up)))
            }
            if let running = refills[context] { return running }
            if initialPending.contains(context) { return schedules[context] }
            return nil
        }
        if let pending {
            await Self.waitUntil(atMost: waitSeconds) { [weak self] in
                guard let self else { return true }
                return self.lock.withLock {
                    pending.isCancelled
                        || !(self.initialPending.contains(context) || self.refills[context] != nil)
                        || (self.stock[context]?.isEmpty == false)
                }
            }
        }
        if let token = pop(context, binding: binding) { return token }
        let failure: AnonTokenUnavailable? = lock.withLock {
            // The account changed while this request waited: its tokens are
            // another account's now, and so is any failure.
            generation == binding ? lastFailure[context] : AnonTokenUnavailable.noSession
        }
        throw failure ?? AnonTokenUnavailable.timedOut
    }

    /// A token for the binding `binding` names, or `nil` once the pool has
    /// been rebound (another account's tokens are never handed out here).
    /// Never starts a refill (ADR-0078 section 8).
    private func pop(_ context: AnonTokenContext, binding: Int) -> AnonToken? {
        lock.withLock {
            guard generation == binding else { return nil }
            return popLocked(context)
        }
    }

    // MARK: - Refill

    /// The refill in flight for `context`, starting one when there is none.
    /// Throws when no refill can start: no session, or the allowance is known
    /// to be used up. Called with the lock held, by the schedule only.
    private func startRefillLocked(_ context: AnonTokenContext) throws -> Task<Void, Never> {
        if let running = refills[context] { return running }
        guard let sessionToken else { throw AnonTokenUnavailable.noSession }
        if let until = quotaUntil[context] {
            let left = until.timeIntervalSince(now())
            if left > 0 {
                throw AnonTokenUnavailable.quotaExhausted(retryAfterSecs: Int(left.rounded(.up)))
            }
            quotaUntil[context] = nil
        }
        pruneLocked(context)
        let held = stock[context]?.count ?? 0
        // A top-up to the target, in one issuance.
        let wanted = min(context.poolTarget - held, Self.maxHeld - held)
        if wanted <= 0 {
            // Full: nothing to fetch. A waiting caller finds a token right away.
            return Task {}
        }
        let generation = self.generation
        let epoch = currentEpochLocked()
        let issuer = self.issuer
        let task = Task { [weak self] in
            guard let self else { return }
            do {
                let tokens = try await self.issueOnce(
                    issuer: issuer,
                    context: context,
                    epoch: epoch,
                    count: UInt32(wanted),
                    sessionToken: sessionToken,
                    generation: generation
                )
                self.finishRefill(context, generation: generation, tokens: tokens, failure: nil)
            } catch let failure as AnonTokenUnavailable {
                self.finishRefill(context, generation: generation, tokens: [], failure: failure)
            } catch {
                self.finishRefill(
                    context, generation: generation, tokens: [],
                    failure: .issuanceFailed(String(describing: error)))
            }
        }
        refills[context] = task
        return task
    }

    /// One issuance, rebuilt ONCE for the issuer's epoch on `epoch-not-issuable`.
    private func issueOnce(
        issuer: AnonTokenIssuing,
        context: AnonTokenContext,
        epoch: UInt64,
        count: UInt32,
        sessionToken: String,
        generation: Int
    ) async throws -> [AnonToken] {
        do {
            return try await issuer.issue(
                context: context, epoch: epoch, count: count, sessionToken: sessionToken)
        } catch AnonTokenUnavailable.epochNotIssuable(let current) {
            learnEpoch(current, generation: generation)
            return try await issuer.issue(
                context: context, epoch: current, count: count, sessionToken: sessionToken)
        }
    }

    private func finishRefill(
        _ context: AnonTokenContext,
        generation: Int,
        tokens: [AnonToken],
        failure: AnonTokenUnavailable?
    ) {
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation else { return }
        refills[context] = nil
        if let failure {
            lastFailure[context] = failure
            if case .quotaExhausted(let retryAfter) = failure {
                quotaUntil[context] = now().addingTimeInterval(TimeInterval(retryAfter ?? 60))
            }
            NSLog("[anon-token] %@ refill failed: %@", context.rawValue, String(describing: failure))
            return
        }
        lastFailure[context] = nil
        var held = stock[context] ?? []
        held.append(contentsOf: tokens.filter { $0.context == context.rawValue })
        if held.count > Self.maxHeld { held.removeFirst(held.count - Self.maxHeld) }
        stock[context] = held
        persistLocked(context)
    }

    private func learnEpoch(_ issuerEpoch: UInt64, generation: Int) {
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation else { return }
        let local = localEpochLocked()
        epochCorrection = Int64(bitPattern: issuerEpoch &- local)
    }

    // MARK: - Epochs

    /// The epoch tokens are spent against now.
    public func currentEpoch() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return currentEpochLocked()
    }

    private func localEpochLocked() -> UInt64 {
        let secs = UInt64(max(0, now().timeIntervalSince1970))
        return (try? anonTokenEpochAt(unixSecs: secs, windowSecs: windowSecs)) ?? 0
    }

    private func currentEpochLocked() -> UInt64 {
        localEpochLocked() &+ UInt64(bitPattern: epochCorrection)
    }

    private func pruneLocked(_ context: AnonTokenContext) {
        guard let held = stock[context], !held.isEmpty else { return }
        let current = currentEpochLocked()
        stock[context] = held.filter { anonTokenIsLive(tokenEpoch: $0.epoch, currentEpoch: current) }
    }

    private func popLocked(_ context: AnonTokenContext) -> AnonToken? {
        pruneLocked(context)
        guard var held = stock[context], !held.isEmpty else { return nil }
        let token = held.removeFirst()
        stock[context] = held
        // Written before the token leaves the pool: a token handed out is
        // never handed out again after a relaunch.
        persistLocked(context)
        return token
    }

    /// Write `context`'s tokens for the bound account to the store.
    private func persistLocked(_ context: AnonTokenContext) {
        guard let store, let accountKey, !forgotten else { return }
        store.save(accountKey: accountKey, context: context, tokens: stock[context] ?? [])
    }

    /// Wait until `done()` answers true, checking every 20 ms, but no longer
    /// than `seconds`. The refill itself keeps running when the wait gives up.
    private static func waitUntil(
        atMost seconds: TimeInterval,
        _ done: @escaping @Sendable () -> Bool
    ) async {
        let deadline = Date().addingTimeInterval(max(0, seconds))
        while !done(), Date() < deadline {
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
    }
}

/// Anonymous tokens drawn with a session that has NOT landed on this device,
/// one per ``take(_:)``, never pooled and never stored.
///
/// ADR-0056 security pass: a join-by-name restore checks the typed name at
/// discovery with the freshly redeemed session BEFORE anything is persisted.
/// Binding ``AnonTokenPool/shared`` to that session would write tokens to the
/// Keychain for an account the restore may still refuse; this source holds
/// nothing, so a refusal leaves no token behind. Each call issues exactly the
/// one token the request spends (not a pool batch), rebuilt once for the
/// issuer's epoch on `epoch-not-issuable`.
public final class DetachedAnonTokenSource: AnonTokenSource, @unchecked Sendable {

    private let issuer: AnonTokenIssuing
    private let sessionToken: String
    private let now: @Sendable () -> Date
    private let windowSecs: UInt64

    public init(
        issuer: AnonTokenIssuing,
        sessionToken: String,
        now: @escaping @Sendable () -> Date = { Date() },
        windowSecs: UInt64 = anonTokenDefaultWindowSecs()
    ) {
        self.issuer = issuer
        self.sessionToken = sessionToken
        self.now = now
        self.windowSecs = windowSecs
    }

    public func take(_ context: AnonTokenContext) async throws -> AnonToken {
        let secs = UInt64(max(0, now().timeIntervalSince1970))
        let epoch = (try? anonTokenEpochAt(unixSecs: secs, windowSecs: windowSecs)) ?? 0
        let tokens: [AnonToken]
        do {
            tokens = try await issuer.issue(
                context: context, epoch: epoch, count: 1, sessionToken: sessionToken)
        } catch AnonTokenUnavailable.epochNotIssuable(let current) {
            tokens = try await issuer.issue(
                context: context, epoch: current, count: 1, sessionToken: sessionToken)
        }
        guard let token = tokens.first(where: { $0.context == context.rawValue }) else {
            throw AnonTokenUnavailable.issuanceFailed("the issuer returned no token")
        }
        return token
    }
}
