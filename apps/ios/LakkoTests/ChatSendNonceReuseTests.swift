// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// Ordering guard for the MLS application-message nonce-reuse-on-crash
/// fix (iOS mirror of the web `apps/web/src/chat/client.ts`
/// `persistDurableOrThrow` gate).
///
/// The bug: `ChatClient.sendMessage` used to publish the generation-N
/// ciphertext to the DS BEFORE the (N+1) ratchet snapshot was durably on
/// disk, and the persist was best-effort. A crash between publish and
/// persist reloads the ratchet at N → the next send re-consumes gen N →
/// identical AEAD (key, nonce) on a different plaintext (plaintext leak +
/// Poly1305 forgery).
///
/// The fix threads a FAIL-CLOSED `ratchetPersister` into `sendMessage`
/// that runs AFTER `createApplicationMessage` (ratchet now N+1) and
/// BEFORE the DS publish; if it throws, `sendMessage` throws and never
/// publishes.
///
/// These tests drive a REAL MLS `Group` (so `createApplicationMessage`
/// actually advances the ratchet, reaching the gate) against a
/// `DsClient` whose `URLSession` is intercepted by ``RecordingURLProtocol``,
/// so "the publish was never attempted" is asserted directly (zero
/// network requests), not merely inferred from the error message.
final class ChatSendNonceReuseTests: XCTestCase {

    private struct SentinelPersistError: Error {}

    /// A real founder `Group` + its `Identity`. A single-member founder
    /// group is a valid MLS group whose creator can encrypt application
    /// messages, which is all this test needs to reach the persist gate.
    private func makeGroup() throws -> (identity: Identity, group: Group) {
        let seed = Data(repeating: 0x11, count: 32)
        let identity = try Identity.fromSeed(seed: seed)
        let group = try Group.create(identity: identity, kind: .group)
        return (identity, group)
    }

    /// A `DsClient` whose requests are intercepted + recorded by
    /// ``RecordingURLProtocol`` (and always failed), so any publish
    /// attempt is observable.
    private func makeRecordingClient() -> (client: ChatClient, ds: DsClient) {
        RecordingURLProtocol.reset()
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [RecordingURLProtocol.self]
        let session = URLSession(configuration: config)
        let ds = DsClient(baseURL: "http://127.0.0.1:9", session: session)
        return (ChatClient(ds: ds), ds)
    }

    /// A persister that THROWS must abort the send BEFORE any DS publish.
    func testPersistFailureAbortsBeforePublish() async throws {
        let (identity, group) = try makeGroup()
        let (client, _) = makeRecordingClient()
        client.ratchetPersister = { throw SentinelPersistError() }

        do {
            _ = try await client.sendMessage(
                identity: identity,
                group: group,
                plaintext: Data("hello".utf8)
            )
            XCTFail("sendMessage must throw when the ratchet persist fails")
        } catch {
            // The abort must be attributed to the persist gate, not to a
            // downstream DS failure.
            let description = (error as? LocalizedError)?.errorDescription
                ?? String(describing: error)
            XCTAssertTrue(
                description.contains("send aborted to prevent nonce reuse"),
                "expected a persist-abort error, got: \(description)"
            )
        }

        // The load-bearing assertion: the gen-N ciphertext never left the
        // device. Zero requests reached the DS.
        XCTAssertEqual(
            RecordingURLProtocol.startedRequestCount, 0,
            "DS publish must NOT be attempted after a failed ratchet persist"
        )
    }

    /// Control: a persister that SUCCEEDS lets the send proceed to the DS
    /// publish — proving the only thing that stops the publish in the
    /// abort test above is the fail-closed gate, not the test harness.
    func testPersistSuccessProceedsToPublish() async throws {
        let (identity, group) = try makeGroup()
        let (client, _) = makeRecordingClient()
        var persistCalled = false
        client.ratchetPersister = { persistCalled = true }

        do {
            _ = try await client.sendMessage(
                identity: identity,
                group: group,
                plaintext: Data("hello".utf8)
            )
            XCTFail("sendMessage must throw — the recording DS fails every request")
        } catch {
            let description = (error as? LocalizedError)?.errorDescription
                ?? String(describing: error)
            XCTAssertTrue(
                description.contains("DS publish failed"),
                "expected a DS-publish error once the persist succeeded, got: \(description)"
            )
        }

        XCTAssertTrue(persistCalled, "the ratchet persister must run before publish")
        XCTAssertGreaterThanOrEqual(
            RecordingURLProtocol.startedRequestCount, 1,
            "a successful persist must let the send reach the DS publish"
        )
    }
}

/// Records every request that actually starts loading and fails it, so a
/// test can assert whether a network call was attempted.
final class RecordingURLProtocol: URLProtocol {

    private static let lock = NSLock()
    private static var _startedRequestCount = 0

    static var startedRequestCount: Int {
        lock.lock(); defer { lock.unlock() }
        return _startedRequestCount
    }

    static func reset() {
        lock.lock(); defer { lock.unlock() }
        _startedRequestCount = 0
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        RecordingURLProtocol.lock.lock()
        RecordingURLProtocol._startedRequestCount += 1
        RecordingURLProtocol.lock.unlock()
        client?.urlProtocol(
            self,
            didFailWithError: URLError(.notConnectedToInternet)
        )
    }

    override func stopLoading() {}
}
