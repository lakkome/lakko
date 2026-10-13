// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// ``Cancellation``: a task called off because the person left the screen is
/// not a failed request.
@MainActor
final class CancellationTests: XCTestCase {
    private struct Refused: Error {}

    // MARK: - What counts

    func testTheTwoBareShapesAreCancellations() {
        XCTAssertTrue(Cancellation.isCancellation(CancellationError()))
        XCTAssertTrue(Cancellation.isCancellation(URLError(.cancelled)))
    }

    func testARealFailureIsNotACancellation() {
        XCTAssertFalse(Cancellation.isCancellation(Refused()))
        XCTAssertFalse(Cancellation.isCancellation(URLError(.timedOut)))
        XCTAssertFalse(Cancellation.isCancellation(URLError(.notConnectedToInternet)))
        XCTAssertFalse(Cancellation.isCancellation(
            DsClientError.network(endpoint: "/ds/x", underlying: URLError(.networkConnectionLost))))
        XCTAssertFalse(Cancellation.isCancellation(
            ChatError.wrapped("fetch failed", underlying: Refused())))
    }

    func testACancellationIsFoundInsideEveryClientWrapper() {
        let inner = URLError(.cancelled)
        let wrapped: [Error] = [
            DsClientError.network(endpoint: "e", underlying: inner),
            AuthClientError.network(endpoint: "e", underlying: inner),
            DiscoveryClientError.network(endpoint: "e", underlying: inner),
            StorageClientError.network(endpoint: "e", underlying: inner),
            PushClientError.network(endpoint: "e", underlying: inner),
            WebauthnClientError.network(endpoint: "e", underlying: inner),
            TransparencyClientError.network(endpoint: "e", underlying: inner),
            CallJwtClientError.network(endpoint: "e", underlying: inner),
            ContactsError.wrapped(inner),
            AttachmentBridgeError.upload(inner),
            AttachmentBridgeError.download(inner),
        ]
        for error in wrapped {
            XCTAssertTrue(Cancellation.isCancellation(error), "\(type(of: error))")
        }
    }

    func testACancellationIsFoundThroughNestedWrappers() {
        let nested = ChatError.wrapped(
            "group invite fetch failed",
            underlying: DiscoveryClientError.network(
                endpoint: "/discovery/group-invite", underlying: CancellationError()))
        XCTAssertTrue(Cancellation.isCancellation(nested))
    }

    func testAWrapperThatIsNotATransportErrorIsNotFollowed() {
        // A decrypt failure is a failure whatever it carries.
        XCTAssertFalse(Cancellation.isCancellation(
            AttachmentBridgeError.decryption(CancellationError())))
    }

    func testTheInboxPollUsesTheSameRule() {
        XCTAssertTrue(ContactsRepository.isCancellation(
            DsClientError.network(endpoint: "e", underlying: URLError(.cancelled))))
        XCTAssertFalse(ContactsRepository.isCancellation(Refused()))
    }

    // MARK: - A cancelled task

    func testAnyErrorOnACancelledTaskEndedByCancellation() async {
        let onCancelled = await Task { @MainActor () -> Bool in
            withUnsafeCurrentTask { $0?.cancel() }
            return Cancellation.endedByCancellation(Refused())
        }.value
        XCTAssertTrue(onCancelled, "the person left: nothing is reported, whatever the error says")
        XCTAssertFalse(Cancellation.endedByCancellation(Refused()), "a live task reports its failures")
        XCTAssertTrue(Cancellation.endedByCancellation(URLError(.cancelled)))
    }

    /// Cancel mid-request: the URL loading system reports it as
    /// `URLError.cancelled`, which the rule recognises.
    func testARequestCancelledMidFlightIsRecognised() async {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [NeverAnsweringURLProtocol.self]
        let session = URLSession(configuration: config)
        let url = URL(string: "https://never-answers.invalid/")!
        let request = Task { () -> Error? in
            do {
                _ = try await session.data(from: url)
                return nil
            } catch {
                return error
            }
        }
        try? await Task.sleep(nanoseconds: 50_000_000)
        request.cancel()
        guard let error = await request.value else {
            return XCTFail("the request was cancelled, so it threw")
        }
        XCTAssertTrue(Cancellation.isCancellation(error), "\(error)")
    }

    // MARK: - Shielded work

    func testShieldedWorkRunsToItsEndUnderACancelledCaller() async {
        var steps: [String] = []
        var sawCancelled: Bool?
        let caller = Task { @MainActor in
            withUnsafeCurrentTask { $0?.cancel() }
            await Cancellation.shielded {
                steps.append("first write")
                // A cancelled task returns from this at once and would stop here.
                try? await Task.sleep(nanoseconds: 1_000_000)
                sawCancelled = Task.isCancelled
                steps.append("second write")
            }
        }
        await caller.value
        XCTAssertEqual(steps, ["first write", "second write"])
        XCTAssertEqual(sawCancelled, false, "the caller's cancellation does not reach the work")
    }

    func testShieldedWorkReturnsItsValue() async {
        let value = await Cancellation.shielded { 7 }
        XCTAssertEqual(value, 7)
    }
}

/// Takes every request and never answers it, so a request stays in flight
/// until it is cancelled. No network is touched.
private final class NeverAnsweringURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {}
    override func stopLoading() {}
}
