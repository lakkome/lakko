// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
@testable import Lakko

// ADR-0073 R.13b "Test hygiene" - the explicit stub for a client a fixture
// needs only so that something can be constructed.
//
// A default-constructed client (`DiscoveryClient()`, `ChatClient()`,
// `DsClient()`, ...) carries a real transport pointed at the configured
// server. A fixture that is not about that client still has to hand one to a
// repository, and the code under test may well call it (the own-devices
// lookup on every commit, the anonymous-token refill on every session). Such
// a fixture takes its client from here: every request is answered locally
// with `404` and an empty JSON object, which every client reads as "nothing
// there", and is recorded so a test can assert on what was asked.
//
// The suite-wide `NetworkGuard` is what catches a fixture that forgot.

/// Answers every request with `404 {}` and records it.
final class OfflineURLProtocol: URLProtocol {

    private static let lock = NSLock()
    private static var recorded: [String] = []

    /// Every request answered so far, `"<METHOD> <url>"`.
    static var requests: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.lock.lock()
        Self.recorded.append("\(request.httpMethod ?? "GET") \(request.url?.absoluteString ?? "")")
        Self.lock.unlock()
        guard let url = request.url,
              let response = HTTPURLResponse(
                url: url, statusCode: 404, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"])
        else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data("{}".utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

/// A token source for a fixture that is not about tokens: one that never
/// reaches the issuer.
final class OfflineAnonTokens: AnonTokenSource, @unchecked Sendable {
    func take(_ context: AnonTokenContext) async throws -> AnonToken {
        AnonToken(context: context.rawValue, epoch: 0, headerValue: "offline-\(context.rawValue)")
    }
}

/// Clients whose every request is answered locally.
enum Offline {

    /// The hosts are in the reserved `.test` zone: a request that somehow
    /// escaped the stub could still not resolve.
    static let discoveryBaseURL = "http://discovery.offline.test"
    static let dsBaseURL = "http://ds.offline.test"
    static let authBaseURL = "http://auth.offline.test"
    static let storageBaseURL = "http://storage.offline.test"

    static func session() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [OfflineURLProtocol.self]
        return URLSession(configuration: configuration)
    }

    static func discovery(sessionToken: String? = nil) -> DiscoveryClient {
        DiscoveryClient(
            baseURL: discoveryBaseURL, session: session(), sessionToken: sessionToken,
            tokens: OfflineAnonTokens())
    }

    static func ds() -> DsClient {
        DsClient(baseURL: dsBaseURL, session: session(), tokens: OfflineAnonTokens())
    }

    static func chatClient() -> ChatClient {
        ChatClient(ds: ds())
    }

    static func auth() -> AuthClient {
        AuthClient(baseURL: authBaseURL, session: session())
    }

    static func storage() -> StorageClient {
        StorageClient(baseURL: storageBaseURL, session: session())
    }

    /// A backup client whose storage routes and head mint both stay local.
    static func backupClient() -> BackupClient {
        BackupClient(storage: storage(), authBaseURL: authBaseURL, authSession: session())
    }
}

extension Offline {

    /// A push routing whose subscribe and unsubscribe never leave the
    /// process (the production default builds a `PushClient()` per call).
    static func pushRouting() -> PushGroupRouting {
        PushGroupRouting(subscribeUnderPushGid: { _ in }, unsubscribeUnderPushGid: { _ in })
    }
}

/// The discovery directory of a fixture that is not about it: unreachable,
/// as on a device with no connection.
struct OfflineAccountDirectory: AccountDirectory {
    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        throw URLError(.notConnectedToInternet)
    }

    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
    ) async throws {
        throw URLError(.notConnectedToInternet)
    }
}

/// The landing rule's directory of a fixture that is not about it: the
/// production default fetches the account's published identity at every
/// session landing.
struct OfflineRestoreDirectory: DeviceRestoreDirectory {
    func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? {
        throw URLError(.notConnectedToInternet)
    }

    func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? {
        throw URLError(.notConnectedToInternet)
    }
}

extension AuthRepository {

    /// The public initializer's arguments over directories that are never
    /// reached: the production one binds `DiscoveryAccountDirectory()` and
    /// `DiscoveryDeviceRestoreDirectory()`, both of which build a default
    /// `DiscoveryClient` per call.
    @MainActor
    static func offline(
        sessionStore: SessionStore = SessionStore(),
        authClientFactory: @escaping () -> AuthClient = { Offline.auth() },
        persistence: SessionPersistence = KeychainSessionPersistence(),
        nowMs: () -> Int64 = SessionRestore.nowMs
    ) -> AuthRepository {
        AuthRepository(
            sessionStore: sessionStore,
            authClientFactory: authClientFactory,
            persistence: persistence,
            nowMs: nowMs,
            accountDirectory: OfflineAccountDirectory(),
            restoreDirectory: OfflineRestoreDirectory())
    }
}
