// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The build-time API base URL (`LakkoApiBaseUrl`) every native REST client
/// derives its service base URL from, and the signed-in token discovery's
/// rate-limited reads carry.
final class ApiEndpointsTests: XCTestCase {

    // MARK: - Base URL derivation

    func testEveryServiceHangsOffTheBase() {
        let base = "https://test.lakko.me/native"
        XCTAssertEqual(ApiEndpoints.resolve(apiBase: base, service: .auth), "https://test.lakko.me/native/auth")
        XCTAssertEqual(ApiEndpoints.resolve(apiBase: base, service: .ds), "https://test.lakko.me/native/ds")
        XCTAssertEqual(ApiEndpoints.resolve(apiBase: base, service: .storage), "https://test.lakko.me/native/storage")
        XCTAssertEqual(ApiEndpoints.resolve(apiBase: base, service: .discovery), "https://test.lakko.me/native/discovery")
        XCTAssertEqual(ApiEndpoints.resolve(apiBase: base, service: .pushGw), "https://test.lakko.me/native/push-gw")
        XCTAssertEqual(ApiEndpoints.resolve(apiBase: base, service: .channels), "https://test.lakko.me/native/channels")
        XCTAssertEqual(ApiEndpoints.resolve(apiBase: base, service: .gifs), "https://test.lakko.me/native/gifs")
    }

    func testTrailingSlashesAndWhitespaceAreTrimmed() {
        XCTAssertEqual(
            ApiEndpoints.resolve(apiBase: "  https://api.lakko.me///\n", service: .auth),
            "https://api.lakko.me/auth"
        )
    }

    func testEmptyOrMissingBaseFallsBackToLocalDevelopment() {
        for empty in [nil, "", "   ", "/", " // "] as [String?] {
            XCTAssertEqual(ApiEndpoints.resolve(apiBase: empty, service: .auth), "http://localhost:8081")
            XCTAssertEqual(ApiEndpoints.resolve(apiBase: empty, service: .ds), "http://localhost:8082")
            XCTAssertEqual(ApiEndpoints.resolve(apiBase: empty, service: .storage), "http://localhost:8083")
            XCTAssertEqual(ApiEndpoints.resolve(apiBase: empty, service: .discovery), "http://localhost:8084")
            XCTAssertEqual(ApiEndpoints.resolve(apiBase: empty, service: .pushGw), "http://localhost:8085")
            XCTAssertEqual(ApiEndpoints.resolve(apiBase: empty, service: .channels), "http://localhost:8086")
            XCTAssertEqual(ApiEndpoints.resolve(apiBase: empty, service: .gifs), "http://localhost:8091")
        }
    }

    func testClientDefaultsFollowTheBuildConfiguration() {
        let base = ApiEndpoints.configuredApiBaseURL
        XCTAssertEqual(AuthClient.defaultBaseURL, ApiEndpoints.resolve(apiBase: base, service: .auth))
        XCTAssertEqual(WebauthnClient.defaultBaseURL, AuthClient.defaultBaseURL)
        XCTAssertEqual(CallJwtClient.defaultBaseURL, AuthClient.defaultBaseURL)
        XCTAssertEqual(DsClient.defaultBaseURL, ApiEndpoints.resolve(apiBase: base, service: .ds))
        XCTAssertEqual(StorageClient.defaultBaseURL, ApiEndpoints.resolve(apiBase: base, service: .storage))
        XCTAssertEqual(DiscoveryClient.defaultBaseURL, ApiEndpoints.resolve(apiBase: base, service: .discovery))
        XCTAssertEqual(PushClient.defaultBaseURL, ApiEndpoints.resolve(apiBase: base, service: .pushGw))
        XCTAssertEqual(ChannelsClient.defaultBaseURL, ApiEndpoints.resolve(apiBase: base, service: .channels))
    }

    // MARK: - Discovery rate-limit token

    /// Records the `Authorization` header per path; answers each route with
    /// the smallest body its client decodes.
    final class HeaderRecorder: URLProtocol {
        private static let lock = NSLock()
        private static var _headers: [String: String?] = [:]

        static func reset() {
            lock.lock(); defer { lock.unlock() }
            _headers = [:]
        }

        static func authorization(for path: String) -> String?? {
            lock.lock(); defer { lock.unlock() }
            return _headers[path]
        }

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        override func startLoading() {
            let path = request.url?.path ?? ""
            Self.lock.lock()
            Self._headers[path] = .some(request.value(forHTTPHeaderField: "Authorization"))
            Self.lock.unlock()

            let status: Int
            let body: String
            switch path {
            case "/discovery/oprf": status = 200; body = #"{"evaluated":"AA=="}"#
            case "/discovery/register": status = 200; body = #"{"ok":true}"#
            default: status = 404; body = ""
            }
            let response = HTTPURLResponse(
                url: request.url!, // swiftlint:disable:this force_unwrapping
                statusCode: status,
                httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"]
            )!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }

        override func stopLoading() {}
    }

    private func discovery(sessionToken: String?) -> DiscoveryClient {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [HeaderRecorder.self]
        return DiscoveryClient(
            baseURL: "https://discovery.test",
            session: URLSession(configuration: configuration),
            sessionToken: sessionToken,
            antiCensorship: .disarmed,
            tokens: FakeAnonTokens()
        )
    }

    /// ADR-0054 S3 (reversing the old ambient-token rule): the lookup reads
    /// never carry a session, not even the client's own - a lookup must not
    /// name the account that made it. A protected write still carries it.
    func testAnonymousReadsNeverCarryASessionAndWritesStillDo() async throws {
        HeaderRecorder.reset()
        let client = discovery(sessionToken: "own")

        _ = try await client.oprfEvaluate(OprfRequest(blinded: "AA=="))
        _ = try await client.lookupRaw(LookupRequest(oprfHash: "AA=="))
        _ = try await client.channelLookupRaw(ChannelLookupRequest(oprfHash: "AA=="))
        _ = try await client.register(RegisterRequest(accountId: "AA==", oprfHash: "AA=="))

        XCTAssertEqual(HeaderRecorder.authorization(for: "/discovery/oprf"), .some(nil))
        XCTAssertEqual(HeaderRecorder.authorization(for: "/discovery/lookup"), .some(nil))
        XCTAssertEqual(HeaderRecorder.authorization(for: "/discovery/channel/lookup"), .some(nil))
        XCTAssertEqual(HeaderRecorder.authorization(for: "/discovery/register"), "Bearer own")
    }

    func testAmbientTokenTreatsEmptyAsSignedOut() {
        let holder = AmbientSessionToken()
        holder.set("t")
        XCTAssertEqual(holder.current, "t")
        holder.set("")
        XCTAssertNil(holder.current)
        holder.set("t")
        holder.set(nil)
        XCTAssertNil(holder.current)
    }
}
