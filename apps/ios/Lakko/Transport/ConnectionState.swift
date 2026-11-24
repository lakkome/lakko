// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Network
import UIKit

/// The three states of `docs/ux/connection-state.md` section 1.
public enum ConnectionPhase: Equatable, Sendable {
    case online
    case waiting
    case connecting

    /// The catalog key for the header, or `nil` when the normal title or
    /// subtitle stands.
    public var headerKey: String? {
        switch self {
        case .online: return nil
        case .waiting: return "connection.waiting"
        case .connecting: return "connection.connecting"
        }
    }

    /// The catalog key spoken on entering this phase (section 5).
    public var announcementKey: String {
        switch self {
        case .online: return "connection.restored_a11y"
        case .waiting: return "connection.waiting"
        case .connecting: return "connection.connecting"
        }
    }
}

/// What can happen to the state (section 3).
public enum ConnectionEvent: Equatable, Sendable {
    /// A request got no HTTP answer.
    case networkFailure
    /// A request got an HTTP answer, any status.
    case answered
    /// The system says there is no usable route.
    case osNoRoute
    /// The system says a usable route is back.
    case osRouteBack
}

/// Whether this device can reach the Lakko servers - the one source behind the
/// "Waiting for network..." / "Connecting..." header
/// (`docs/ux/connection-state.md`).
///
/// Fed by `AntiCensorshipTransport.performOnWire`, which every REST request of
/// the app passes through (direct or tunnelled), and by `NWPathMonitor`. The
/// system is an early hint, never the last word: an HTTP answer always
/// returns the state to online. (The simulator keeps reporting a route when
/// the host network is gone; the requests are what notice.)
@MainActor
public final class ConnectionState: ObservableObject {
    public static let shared = ConnectionState(monitorPath: true)

    /// Posted on the main actor when the network comes back (Waiting ->
    /// Connecting), for receive loops that want to end their backoff early.
    public static let networkReturned = Notification.Name("lakko.connection.networkReturned")

    @Published public private(set) var phase: ConnectionPhase = .online

    /// The phase last entered by a transition; `nil` until the first one.
    public private(set) var lastAnnounced: ConnectionPhase?

    /// Where announcements go; replaceable so a test can read them.
    var announce: (String) -> Void = { text in
        UIAccessibility.post(notification: .announcement, argument: text)
    }

    private var monitor: NWPathMonitor?
    private var lastPathSatisfied: Bool?

    init(monitorPath: Bool) {
        guard monitorPath else { return }
        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { path in
            let satisfied = path.status == .satisfied
            Task { @MainActor in ConnectionState.shared.notePath(satisfied: satisfied) }
        }
        monitor.start(queue: DispatchQueue(label: "me.lakko.connection-state"))
        self.monitor = monitor
    }

    /// The transition table of section 3. Pure.
    public nonisolated static func next(_ phase: ConnectionPhase, _ event: ConnectionEvent) -> ConnectionPhase {
        switch event {
        case .networkFailure, .osNoRoute: return .waiting
        case .answered: return .online
        case .osRouteBack: return phase == .waiting ? .connecting : phase
        }
    }

    /// Whether a failed request got no HTTP answer: a `URLError` of the
    /// transport kind (no route, DNS, refused, lost, TLS, timeout) or the
    /// anti-censorship tunnel failing. A cancellation is not one, nor is
    /// anything a caller threw itself. Pure.
    public nonisolated static func isNetworkFailure(_ error: Error) -> Bool {
        if error is CancellationError { return false }
        if let ffi = error as? FfiError, case .Transport = ffi { return true }
        guard let url = error as? URLError else { return false }
        switch url.code {
        case .notConnectedToInternet, .networkConnectionLost, .timedOut,
             .cannotFindHost, .cannotConnectToHost, .dnsLookupFailed,
             .secureConnectionFailed, .serverCertificateUntrusted,
             .serverCertificateHasBadDate, .serverCertificateNotYetValid,
             .serverCertificateHasUnknownRoot, .clientCertificateRejected,
             .cannotLoadFromNetwork, .internationalRoamingOff, .dataNotAllowed,
             .callIsActive, .backgroundSessionWasDisconnected:
            return true
        default:
            return false
        }
    }

    /// Feed one event; a transition to another phase is announced once.
    public func note(_ event: ConnectionEvent) {
        let after = Self.next(phase, event)
        guard after != phase else { return }
        phase = after
        lastAnnounced = after
        announce(Localization.text(after.announcementKey))
        if after == .connecting {
            NetworkReturnSignal.shared.fire()
            NotificationCenter.default.post(name: Self.networkReturned, object: nil)
        }
    }

    /// The system's path changed. Only a change of reachability is an event:
    /// the monitor reports other path changes too.
    func notePath(satisfied: Bool) {
        defer { lastPathSatisfied = satisfied }
        if !satisfied {
            note(.osNoRoute)
        } else if lastPathSatisfied == false {
            note(.osRouteBack)
        }
    }

    /// Record a request that got an HTTP answer, from any thread.
    public nonisolated static func reportAnswered() {
        Task { @MainActor in ConnectionState.shared.note(.answered) }
    }

    /// Record a failed request, from any thread; only a network failure
    /// changes anything.
    public nonisolated static func reportFailure(_ error: Error) {
        guard isNetworkFailure(error) else { return }
        Task { @MainActor in ConnectionState.shared.note(.networkFailure) }
    }
}

/// The network-returned event (Waiting -> Connecting) as something a sleep can
/// wait on, from any thread (`docs/ux/connection-state.md` section 3: every
/// receive loop retries at once instead of finishing its backoff).
///
/// A counter plus waiters: a sleep notes the counter before it starts, and a
/// return that happened between noting it and waiting is still seen, so no
/// return is lost to the race a bare notification observer would have.
final class NetworkReturnSignal: @unchecked Sendable {
    static let shared = NetworkReturnSignal()

    private let lock = NSLock()
    private var generation: UInt64 = 0
    private var waiters: [UUID: CheckedContinuation<Void, Never>] = [:]

    /// How many times the network has come back.
    var current: UInt64 { lock.withLock { generation } }

    /// The network came back: every waiter returns.
    func fire() {
        let resumed: [CheckedContinuation<Void, Never>] = lock.withLock {
            generation &+= 1
            let all = Array(waiters.values)
            waiters.removeAll()
            return all
        }
        resumed.forEach { $0.resume() }
    }

    /// Returns once the network has come back after `since` (at once when it
    /// already has), or when the calling task is cancelled.
    func wait(since: UInt64) async {
        let id = UUID()
        await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let now: Bool = lock.withLock {
                    if generation != since || Task.isCancelled { return true }
                    waiters[id] = continuation
                    return false
                }
                if now { continuation.resume() }
            }
        } onCancel: {
            let continuation = lock.withLock { waiters.removeValue(forKey: id) }
            continuation?.resume()
        }
    }
}

extension ConnectionState {
    /// Sleep `ms` milliseconds as a receive loop's failure backoff: returns
    /// early when the network comes back (`true`) or the task is cancelled.
    /// Never throws. Successful-poll pacing and unrelated timers keep a plain
    /// sleep.
    @discardableResult
    nonisolated static func sleepUnlessNetworkReturns(
        ms: UInt64, signal: NetworkReturnSignal = .shared
    ) async -> Bool {
        let since = signal.current
        guard ms > 0 else { return false }
        await withTaskGroup(of: Void.self) { group in
            group.addTask { try? await Task.sleep(nanoseconds: ms.multipliedReportingOverflow(by: 1_000_000).partialValue) }
            group.addTask { await signal.wait(since: since) }
            _ = await group.next()
            group.cancelAll()
        }
        return signal.current != since
    }
}
