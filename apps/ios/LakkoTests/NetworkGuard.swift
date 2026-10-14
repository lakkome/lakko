// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import ObjectiveC
import XCTest
import LakkoCore
@testable import Lakko

// ADR-0073 R.13b "Test hygiene" - no unit test may reach the network.
//
// Two runs of an earlier suite published MLS ciphertext to the test stack's
// Delivery Service because a fixture built `ChatClient()`, whose default
// `DsClient` carries a real transport. One fixture was fixed; this file makes
// the rule hold for the whole bundle, so the next default-transport client
// fails its test instead of writing to a server.
//
// The guard is installed once, when the test bundle loads (it is the bundle's
// principal class, see `project.yml`), and stays for the process:
//
// 1. `URLSession.shared` - `URLProtocol.registerClass` puts
//    `NetworkGuardURLProtocol` in front of the system loaders.
// 2. Every session built from a configuration (`URLSession(configuration:)`,
//    with or without a delegate) - the two `NSURLSession` factories are
//    swizzled to append the guard AFTER the configuration's own protocol
//    classes. A test's stub protocol therefore answers first; a request the
//    stub declines, and every request of a session that installed no stub,
//    reaches the guard instead of the network.
// 3. WebSocket tasks - they never consult `URLProtocol`, so the three
//    `webSocketTask` factories are swizzled: the violation is recorded and
//    the task is pointed at a closed loopback port.
//
// A request the guard catches FAILS (the caller sees `URLError`), is logged
// with the marker `NETWORK-GUARD`, and is recorded as a failure of the test
// that was running when it was made. A request made while no test runs (the
// host app's own launch) is only refused and logged.
//
// One process-wide default is replaced for the same reason: the shared
// anonymous-token pool, which every client built without a token source
// draws from, gets an issuer that never leaves the process
// (`OfflineAnonTokenIssuer`). A session landed by one test would otherwise
// make the next token route of ANY later test ask the real issuer for a
// refill. Likewise the backup client `AuthRepository` builds for itself at a
// session landing (`AuthRepository.makeBackupClient`), which no fixture can
// pass in.
//
// What it cannot see: sockets opened by the Rust core (the ADR-0026 tunnel
// and the DoH bootstrap fetch). Those run only when anti-censorship is armed,
// which needs a pinned trust anchor no test build carries.
//
// Opt-in: the end-to-end tests that are MEANT to reach a server name it in
// `LAKKO_SFU_E2E_URL`; that host alone is let through.

/// The principal class of the test bundle: XCTest instantiates it once,
/// before any test runs.
@objc(LakkoTestsNetworkGuard)
final class NetworkGuard: NSObject, XCTestObservation {

    /// The marker every caught request is logged with.
    static let logMarker = "NETWORK-GUARD"

    private static let lock = NSLock()
    private static var installed = false
    private static var storeGateReset: StoreGateTestReset?
    private static var current: XCTestCase?
    private static var caught: [String] = []
    private static var caughtInCurrent: [String] = []

    override init() {
        super.init()
        Self.install()
        XCTestObservationCenter.shared.addTestObserver(self)
        // One bundle has one principal class. The store gate's reset before
        // every test (`StoreGateTestReset`, `AccountStoreGateTests.swift`)
        // is installed from here.
        Self.storeGateReset = StoreGateTestReset()
    }

    /// Install the guard. Idempotent.
    static func install() {
        lock.lock()
        defer { lock.unlock() }
        guard !installed else { return }
        installed = true
        AnonTokenPool.shared.setIssuer(OfflineAnonTokenIssuer())
        Task { @MainActor in AuthRepository.makeBackupClient = { Offline.backupClient() } }
        URLProtocol.registerClass(NetworkGuardURLProtocol.self)
        exchangeClassMethod("sessionWithConfiguration:", "lakkoGuard_sessionWithConfiguration:")
        exchangeClassMethod(
            "sessionWithConfiguration:delegate:delegateQueue:",
            "lakkoGuard_sessionWithConfiguration:delegate:delegateQueue:")
        exchangeInstanceMethod("webSocketTaskWithURL:", "lakkoGuard_webSocketTaskWithURL:")
        exchangeInstanceMethod("webSocketTaskWithURL:protocols:", "lakkoGuard_webSocketTaskWithURL:protocols:")
        exchangeInstanceMethod("webSocketTaskWithRequest:", "lakkoGuard_webSocketTaskWithRequest:")
        NSLog("\(logMarker) installed")
    }

    private static func exchangeClassMethod(_ original: String, _ replacement: String) {
        guard let a = class_getClassMethod(URLSession.self, NSSelectorFromString(original)),
              let b = class_getClassMethod(URLSession.self, NSSelectorFromString(replacement))
        else {
            NSLog("\(logMarker) could not guard +[NSURLSession \(original)]")
            return
        }
        method_exchangeImplementations(a, b)
    }

    private static func exchangeInstanceMethod(_ original: String, _ replacement: String) {
        guard let a = class_getInstanceMethod(URLSession.self, NSSelectorFromString(original)),
              let b = class_getInstanceMethod(URLSession.self, NSSelectorFromString(replacement))
        else {
            NSLog("\(logMarker) could not guard -[NSURLSession \(original)]")
            return
        }
        method_exchangeImplementations(a, b)
    }

    // MARK: - What is let through

    /// Where a refused WebSocket is pointed: the discard port on loopback,
    /// which nothing listens on.
    static let refusedSocketHost = "127.0.0.1"
    static let refusedSocketPort = 9
    static let refusedSocketPath = "/network-guard"
    static let refusedSocket = URL(string: "ws://127.0.0.1:9/network-guard")!

    /// The hosts a test run explicitly asked to reach (the SFU end-to-end
    /// tests); empty on an ordinary run.
    static let allowedHosts: Set<String> = {
        var out: Set<String> = []
        if let raw = ProcessInfo.processInfo.environment["LAKKO_SFU_E2E_URL"],
           let host = URL(string: raw)?.host, !host.isEmpty {
            out.insert(host.lowercased())
        }
        return out
    }()

    /// Whether `url` is refused. Only schemes that leave the process are
    /// judged; `file:`, `data:` and `about:` are not network.
    static func refuses(_ url: URL?) -> Bool {
        guard let url, let scheme = url.scheme?.lowercased() else { return false }
        // Where the guard itself points a refused WebSocket (one factory
        // calls another inside Foundation).
        if url.host == refusedSocketHost, url.port == refusedSocketPort, url.path == refusedSocketPath {
            return false
        }
        guard ["http", "https", "ws", "wss"].contains(scheme) else { return false }
        if let host = url.host?.lowercased(), allowedHosts.contains(host) { return false }
        return true
    }

    // MARK: - Recording

    /// Record one caught request against the running test.
    ///
    /// The request is refused on a URL-loading thread, where XCTest does not
    /// accept an issue (and a test that already ended accepts none at all),
    /// so the failure itself is raised by the teardown block
    /// ``testCaseWillStart(_:)`` gave the test: on the test's own thread,
    /// while it is still running.
    static func record(method: String, url: URL?) {
        let line = "\(method) \(url?.absoluteString ?? "<no url>")"
        lock.lock()
        let name = current?.name ?? "<no test running>"
        caught.append("\(name): \(line)")
        if current != nil { caughtInCurrent.append(line) }
        lock.unlock()
        NSLog("\(logMarker) refused an unstubbed request during \(name): \(line)")
    }

    /// The requests caught since the running test started; emptied.
    private static func takeCaughtInCurrent() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        let out = caughtInCurrent
        caughtInCurrent = []
        return out
    }

    private static func withLock<T>(_ body: () -> T) -> T {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }

    /// Every request caught so far, `"<test>: <METHOD> <url>"`.
    static var caughtRequests: [String] {
        lock.lock()
        defer { lock.unlock() }
        return caught
    }

    /// Run `body` with the recording detached from the running test, and
    /// return what the guard caught meanwhile. For the guard's own tests.
    static func capturing(_ body: () async throws -> Void) async rethrows -> [String] {
        let saved: XCTestCase? = withLock {
            let saved = current
            current = nil
            return saved
        }
        let before = withLock { caught.count }
        defer { withLock { current = saved } }
        try await body()
        return withLock { Array(caught[before...]) }
    }

    // MARK: - XCTestObservation

    func testCaseWillStart(_ testCase: XCTestCase) {
        Self.lock.lock()
        Self.current = testCase
        Self.caughtInCurrent = []
        Self.lock.unlock()
        testCase.addTeardownBlock { [weak testCase] in
            let lines = Self.takeCaughtInCurrent()
            guard !lines.isEmpty else { return }
            // A class that stops at its first failure would abort the whole
            // test process from inside a teardown block; the guard's failure
            // is reported and the run goes on.
            testCase?.continueAfterFailure = true
            for line in lines {
                XCTFail("network guard: a unit test made an unstubbed request (\(line))")
            }
        }
    }

    func testCaseDidFinish(_ testCase: XCTestCase) {
        Self.lock.lock()
        if Self.current === testCase { Self.current = nil }
        Self.lock.unlock()
    }

    func testBundleDidFinish(_ testBundle: Bundle) {
        let all = Self.caughtRequests
        NSLog("\(Self.logMarker) summary: \(all.count) unstubbed request(s) refused")
    }
}

/// The protocol that answers every request no stub claimed.
final class NetworkGuardURLProtocol: URLProtocol {

    override class func canInit(with request: URLRequest) -> Bool {
        NetworkGuard.refuses(request.url)
    }

    override class func canInit(with task: URLSessionTask) -> Bool {
        NetworkGuard.refuses(task.currentRequest?.url ?? task.originalRequest?.url)
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        NetworkGuard.record(method: request.httpMethod ?? "GET", url: request.url)
        client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
    }

    override func stopLoading() {}
}

extension URLSession {

    /// A copy of `configuration` whose protocol classes END with the guard.
    private static func lakkoGuarded(_ configuration: URLSessionConfiguration) -> URLSessionConfiguration {
        var classes = configuration.protocolClasses ?? []
        if !classes.contains(where: { $0 == NetworkGuardURLProtocol.self }) {
            // After the caller's own stubs and BEFORE the system loaders the
            // default list carries: a stub answers first, the guard second,
            // the network never.
            let firstSystem = classes.firstIndex { NSStringFromClass($0).hasPrefix("_NS") } ?? classes.count
            classes.insert(NetworkGuardURLProtocol.self, at: firstSystem)
        }
        configuration.protocolClasses = classes
        return configuration
    }

    // After the exchange each of these names the ORIGINAL implementation,
    // so the self-call below is not a recursion.

    @objc(lakkoGuard_sessionWithConfiguration:)
    dynamic class func lakkoGuardSession(configuration: URLSessionConfiguration) -> URLSession {
        lakkoGuardSession(configuration: lakkoGuarded(configuration))
    }

    @objc(lakkoGuard_sessionWithConfiguration:delegate:delegateQueue:)
    dynamic class func lakkoGuardSession(
        configuration: URLSessionConfiguration, delegate: URLSessionDelegate?, delegateQueue: OperationQueue?
    ) -> URLSession {
        lakkoGuardSession(
            configuration: lakkoGuarded(configuration), delegate: delegate, delegateQueue: delegateQueue)
    }

    @objc(lakkoGuard_webSocketTaskWithURL:)
    dynamic func lakkoGuardWebSocketTask(url: URL) -> URLSessionWebSocketTask {
        guard NetworkGuard.refuses(url) else { return lakkoGuardWebSocketTask(url: url) }
        NetworkGuard.record(method: "WEBSOCKET", url: url)
        return lakkoGuardWebSocketTask(url: NetworkGuard.refusedSocket)
    }

    @objc(lakkoGuard_webSocketTaskWithURL:protocols:)
    dynamic func lakkoGuardWebSocketTask(url: URL, protocols: [String]) -> URLSessionWebSocketTask {
        guard NetworkGuard.refuses(url) else { return lakkoGuardWebSocketTask(url: url, protocols: protocols) }
        NetworkGuard.record(method: "WEBSOCKET", url: url)
        return lakkoGuardWebSocketTask(url: NetworkGuard.refusedSocket, protocols: protocols)
    }

    @objc(lakkoGuard_webSocketTaskWithRequest:)
    dynamic func lakkoGuardWebSocketTask(request: URLRequest) -> URLSessionWebSocketTask {
        guard NetworkGuard.refuses(request.url) else { return lakkoGuardWebSocketTask(request: request) }
        NetworkGuard.record(method: "WEBSOCKET", url: request.url)
        return lakkoGuardWebSocketTask(request: URLRequest(url: NetworkGuard.refusedSocket))
    }
}

/// The issuer the shared anonymous-token pool draws from under test: none.
/// A token route of a client that named no token source fails as it does on
/// a device with no connection.
struct OfflineAnonTokenIssuer: AnonTokenIssuing {
    func issue(
        context: AnonTokenContext, epoch: UInt64, count: UInt32, sessionToken: String
    ) async throws -> [AnonToken] {
        throw AnonTokenUnavailable.issuanceFailed("no issuer under test")
    }
}
