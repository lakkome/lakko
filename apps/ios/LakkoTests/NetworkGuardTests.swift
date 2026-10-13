// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073 R.13b "Test hygiene" - the suite-wide network guard
/// (`NetworkGuard.swift`) refuses every request no stub claimed.
final class NetworkGuardTests: XCTestCase {

    private final class Stub: URLProtocol {
        override class func canInit(with request: URLRequest) -> Bool {
            request.url?.path == "/stubbed"
        }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
        override func startLoading() {
            let response = HTTPURLResponse(
                url: request.url!, statusCode: 204, httpVersion: "HTTP/1.1", headerFields: nil)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocolDidFinishLoading(self)
        }
        override func stopLoading() {}
    }

    private let unstubbed = URL(string: "https://network-guard.invalid/unstubbed")!

    private func refused(_ session: URLSession, _ url: URL) async -> Bool {
        do {
            _ = try await session.data(from: url)
            return false
        } catch {
            return (error as? URLError)?.code == .notConnectedToInternet
        }
    }

    func testThePrincipalClassInstalledTheGuard() {
        XCTAssertEqual(
            Bundle(for: NetworkGuardTests.self).object(forInfoDictionaryKey: "NSPrincipalClass") as? String,
            "LakkoTestsNetworkGuard")
    }

    func testTheSharedSessionIsRefused() async {
        var wasRefused = false
        let caught = await NetworkGuard.capturing {
            wasRefused = await self.refused(.shared, self.unstubbed)
        }
        XCTAssertTrue(wasRefused)
        XCTAssertEqual(caught.count, 1)
        XCTAssertTrue(caught.first?.hasSuffix("GET https://network-guard.invalid/unstubbed") == true)
    }

    func testASessionBuiltFromAnyConfigurationIsRefused() async {
        for configuration in [URLSessionConfiguration.default, .ephemeral] {
            var wasRefused = false
            let session = URLSession(configuration: configuration)
            let caught = await NetworkGuard.capturing {
                wasRefused = await self.refused(session, self.unstubbed)
            }
            XCTAssertTrue(wasRefused)
            XCTAssertEqual(caught.count, 1)
        }
    }

    func testASessionWithADelegateIsRefused() async {
        final class Delegate: NSObject, URLSessionDelegate {}
        let session = URLSession(configuration: .ephemeral, delegate: Delegate(), delegateQueue: nil)
        var wasRefused = false
        let caught = await NetworkGuard.capturing {
            wasRefused = await self.refused(session, self.unstubbed)
        }
        XCTAssertTrue(wasRefused)
        XCTAssertEqual(caught.count, 1)
    }

    func testAStubAnswersFirstAndWhatItDeclinesIsRefused() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [Stub.self]
        let session = URLSession(configuration: configuration)
        var status: Int?
        var declinedWasRefused = false
        let caught = try await NetworkGuard.capturing {
            let (_, response) = try await session.data(from: URL(string: "https://network-guard.invalid/stubbed")!)
            status = (response as? HTTPURLResponse)?.statusCode
            declinedWasRefused = await self.refused(session, self.unstubbed)
        }
        XCTAssertEqual(status, 204, "an explicitly stubbed request is answered by its stub")
        XCTAssertTrue(declinedWasRefused, "a request the stub declined never reaches the network")
        XCTAssertEqual(caught.count, 1)
    }

    func testAWebSocketIsRecordedAndPointedAway() async {
        var task: URLSessionWebSocketTask?
        let caught = await NetworkGuard.capturing {
            task = URLSession.shared.webSocketTask(
                with: URL(string: "wss://network-guard.invalid/v1/connect")!, protocols: ["lakko-sfu-v1"])
        }
        XCTAssertEqual(caught.count, 1)
        XCTAssertTrue(caught.first?.contains("WEBSOCKET wss://network-guard.invalid/v1/connect") == true)
        XCTAssertEqual(task?.originalRequest?.url?.host, "127.0.0.1")
        task?.cancel()
    }

    func testOnlyNetworkSchemesAreJudged() {
        XCTAssertTrue(NetworkGuard.refuses(URL(string: "https://test.lakko.me/native/ds/x")))
        XCTAssertTrue(NetworkGuard.refuses(URL(string: "http://localhost:8082/ds/x")))
        XCTAssertTrue(NetworkGuard.refuses(URL(string: "wss://test.lakko.me/sfu")))
        XCTAssertFalse(NetworkGuard.refuses(URL(fileURLWithPath: "/tmp/x")))
        XCTAssertFalse(NetworkGuard.refuses(URL(string: "data:text/plain,x")))
        XCTAssertFalse(NetworkGuard.refuses(nil))
    }
}
