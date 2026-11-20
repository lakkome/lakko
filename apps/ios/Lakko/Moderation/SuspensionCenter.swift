// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0089 step 3 / UX `reporting-and-moderation.md` section 4 — suspended
/// mode for the account signed in on this device.
///
/// Entered when auth answers `403 account-suspended` (a landing, a refresh,
/// or a bearer route followed by a refresh); left silently when a refresh
/// answers a normal pair. While active it shows the suspended screen (or,
/// after "Read my chats", the read-only app), asks auth again on every
/// foreground and every 10 minutes, and sends the one appeal.
///
/// The `s1` and `r2` are the session pair the token provider holds and the
/// Keychain keeps; this object holds a per-account flag
/// (``SuspensionStore``) and what the screen shows.
@MainActor
public final class SuspensionCenter: ObservableObject {

    public static let shared = SuspensionCenter()

    /// What the screen's details are.
    public enum Details: Equatable {
        case loading
        case loaded(SuspensionDetails)
        case failed
    }

    /// The appeal sheet's state.
    public enum AppealPhase: Equatable {
        case editing
        case sending
        /// Sending failed; the text is kept.
        case failed
        /// No moderator list, or no pinned root key.
        case unavailable
    }

    /// The suspended account, or `nil` when no account on this device is in
    /// suspended mode.
    @Published public private(set) var account: String?
    /// The person chose "Read my chats": the app shows read-only instead of
    /// the screen. In memory only, so a restart shows the screen again.
    @Published public private(set) var reading = false
    @Published public private(set) var details: Details = .loading
    @Published public private(set) var appealPhase: AppealPhase = .editing

    /// Whether suspended mode is on.
    public var active: Bool { account != nil }

    /// The seams, injectable for tests.
    public struct Ports {
        /// The `s1` the provider holds now, or `nil`.
        public var restrictedToken: () -> String?
        /// One refresh through the token provider; the provider's callbacks
        /// enter or leave suspended mode.
        public var refresh: () async -> Void
        public var status: (_ token: String) async -> SuspensionClient.StatusResult
        public var sendAppeal: (_ sealed: Data, _ token: String) async -> SuspensionClient.AppealResult
        public var moderatorList: () async -> ModerationClient.ListResult
        public var rootKey: () -> Data?
        public var seal: (_ text: String, _ list: Data, _ rootKey: Data) throws -> Data
        public var nowMs: () -> Int64

        public static var live: Ports {
            let client = SuspensionClient()
            let moderation = ModerationClient()
            return Ports(
                restrictedToken: {
                    let provider = SessionTokenProvider.shared
                    return provider.isSuspended ? provider.current?.sessionToken : nil
                },
                refresh: { _ = await SessionTokenProvider.shared.refresh() },
                status: { await client.status(restrictedToken: $0) },
                sendAppeal: { await client.appeal(sealed: $0, restrictedToken: $1) },
                // The report sheet's gate (``ModeratorListGate``): an appeal is
                // never sealed to a list older than this device's floor.
                moderatorList: {
                    await ModeratorListGate.admittedList(
                        rootKey: ModerationClient.pinnedRootKey,
                        fetch: { await moderation.fetchModeratorList() })
                },
                rootKey: { ModerationClient.pinnedRootKey },
                seal: { try sealModerationAppeal(text: $0, moderatorList: $1, rootKey: $2) },
                nowMs: { Int64(Date().timeIntervalSince1970 * 1000) })
        }
    }

    private let store: SuspensionStore
    private var ports: Ports
    private var lastRefreshMs: Int64?

    public init(store: SuspensionStore = SuspensionStore(), ports: Ports? = nil) {
        self.store = store
        self.ports = ports ?? .live
    }

    // MARK: - Entering and leaving

    /// Auth answered `account-suspended` for `accountIdB64`. Idempotent: a
    /// renewal of the `s1` re-enters without resetting the screen.
    public func enter(accountIdB64: String) {
        store.set(true, accountIdB64: accountIdB64)
        lastRefreshMs = ports.nowMs()
        if account != accountIdB64 {
            account = accountIdB64
            reading = false
            details = .loading
            appealPhase = .editing
            Task { await loadDetails() }
        }
    }

    /// A refresh answered a normal pair: the suspension is over. Silent.
    public func leave(accountIdB64: String) {
        store.set(false, accountIdB64: accountIdB64)
        guard account == accountIdB64 else { return }
        account = nil
        reading = false
        details = .loading
        appealPhase = .editing
        lastRefreshMs = nil
    }

    /// The account bound on this device changed (a landing, a sign-out):
    /// suspended mode follows that account's flag. `nil` turns it off
    /// without forgetting the flag.
    public func bind(accountIdB64: String?) {
        guard let accountIdB64, store.isSuspended(accountIdB64: accountIdB64) else {
            if account != nil {
                account = nil
                reading = false
                details = .loading
                appealPhase = .editing
                lastRefreshMs = nil
            }
            return
        }
        if account != accountIdB64 {
            account = accountIdB64
            reading = false
            details = .loading
            appealPhase = .editing
            lastRefreshMs = nil
        }
    }

    /// Whether `accountIdB64` was in suspended mode when last seen here.
    public func isFlagged(accountIdB64: String) -> Bool {
        store.isSuspended(accountIdB64: accountIdB64)
    }

    /// The account's data is gone from this device: forget the flag.
    public func forget(accountIdB64: String) {
        store.set(false, accountIdB64: accountIdB64)
        if account == accountIdB64 { bind(accountIdB64: nil) }
    }

    // MARK: - The screen

    /// "Read my chats".
    public func readChats() {
        guard active else { return }
        reading = true
    }

    /// The banner at the top of the chat list: the screen again.
    public func showScreen() {
        guard active else { return }
        reading = false
        if case .failed = details { Task { await loadDetails() } }
    }

    /// `GET /account/suspension` with the `s1`.
    public func loadDetails() async {
        guard let current = account else { return }
        guard let token = ports.restrictedToken() else {
            details = .failed
            return
        }
        if case .loaded = details {} else { details = .loading }
        switch await ports.status(token) {
        case .answer(.suspended(let loaded)):
            guard account == current else { return }
            details = .loaded(loaded)
        case .answer(.notSuspended):
            // Auth says it is over: the refresh confirms it and leaves.
            await ports.refresh()
        case .failed:
            guard account == current else { return }
            if case .loaded = details { return }
            details = .failed
        }
    }

    /// Retry after a failed load.
    public func retryDetails() {
        Task { await loadDetails() }
    }

    // MARK: - Asking auth again

    /// The app came to the foreground: ask auth now.
    public func noteForeground() async {
        guard active else { return }
        await refreshNow()
    }

    /// The 10-minute check: ask auth when the interval has passed.
    public func tick() async {
        guard active, SuspensionRefreshRule.due(lastMs: lastRefreshMs, nowMs: ports.nowMs()) else { return }
        await refreshNow()
    }

    /// One refresh, then the details again while still suspended.
    public func refreshNow() async {
        guard active else { return }
        lastRefreshMs = ports.nowMs()
        await ports.refresh()
        if active { await loadDetails() }
    }

    /// Run ``tick()`` once a minute while suspended (the interval itself is
    /// ``SuspensionRefreshRule/intervalMs``). Returns when suspended mode
    /// ends or the task is cancelled.
    public func runChecks() async {
        while active, !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 60 * 1_000_000_000)
            guard !Task.isCancelled else { return }
            await tick()
        }
    }

    // MARK: - Appeal

    /// Whether the screen offers `Appeal`: details loaded and none sent.
    public var canAppeal: Bool {
        if case .loaded(let loaded) = details { return loaded.appeal == .none }
        return false
    }

    /// The sheet opened.
    public func beginAppeal() {
        appealPhase = .editing
    }

    /// Seal and send the appeal. `true` when it is on file (the sheet
    /// closes and the screen shows "Appeal sent").
    @discardableResult
    public func sendAppeal(_ text: String) async -> Bool {
        guard let current = account, SuspensionAppeal.sendable(text), appealPhase != .sending else { return false }
        if case .loaded(let loaded) = details, loaded.appeal != .none { return false }
        guard let token = ports.restrictedToken(), let root = ports.rootKey() else {
            appealPhase = ports.rootKey() == nil ? .unavailable : .failed
            return false
        }
        appealPhase = .sending
        let list: Data
        switch await ports.moderatorList() {
        case .list(let fetched):
            list = fetched
        case .unavailable:
            appealPhase = .unavailable
            return false
        case .failed:
            appealPhase = .failed
            return false
        }
        let sealed: Data
        do {
            sealed = try ports.seal(text, list, root)
        } catch {
            NSLog("[suspension] the appeal could not be sealed")
            appealPhase = .unavailable
            return false
        }
        switch await ports.sendAppeal(sealed, token) {
        case .sent:
            guard account == current else { return false }
            appealPhase = .editing
            if case .loaded(let loaded) = details {
                details = .loaded(loaded.with(appeal: .sent))
            } else {
                await loadDetails()
            }
            return true
        case .notSuspended:
            appealPhase = .editing
            await ports.refresh()
            return true
        case .failed:
            appealPhase = .failed
            return false
        }
    }
}
