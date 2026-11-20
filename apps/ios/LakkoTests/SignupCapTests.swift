// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import XCTest

@testable import Lakko

/// Answers every request with one scripted status and body.
final class SignupOpenStubURLProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var script: (status: Int, body: String) = (404, "{}")
    private static var urls: [String] = []

    static func answer(status: Int, body: String) {
        lock.withLock {
            script = (status, body)
            urls = []
        }
    }

    static var requests: [String] { lock.withLock { urls } }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let (status, body) = Self.lock.withLock { () -> (Int, String) in
            Self.urls.append("\(request.httpMethod ?? "GET") \(request.url?.absoluteString ?? "")")
            return Self.script
        }
        guard let url = request.url,
              let response = HTTPURLResponse(
                url: url, statusCode: status, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"])
        else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

/// The beta account cap (`docs/ux/beta-account-cap.md`).
@MainActor
final class SignupCapTests: XCTestCase {

    private func client() -> AuthClient {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [SignupOpenStubURLProtocol.self]
        return AuthClient(baseURL: Offline.authBaseURL, session: URLSession(configuration: configuration))
    }

    // MARK: - GET /signup/open

    func testOpenTrueIsOpenAndAsksTheRightRoute() async {
        SignupOpenStubURLProtocol.answer(status: 200, body: #"{"open":true}"#)
        let open = await client().signupOpen()
        XCTAssertTrue(open)
        XCTAssertEqual(SignupOpenStubURLProtocol.requests, ["GET \(Offline.authBaseURL)/signup/open"])
    }

    func testOpenFalseIsClosed() async {
        SignupOpenStubURLProtocol.answer(status: 200, body: #"{"open":false}"#)
        let open = await client().signupOpen()
        XCTAssertFalse(open)
    }

    func testAnOlderServerAnswering404IsOpen() async {
        SignupOpenStubURLProtocol.answer(status: 404, body: "{}")
        let open = await client().signupOpen()
        XCTAssertTrue(open)
    }

    func testAServerErrorIsOpen() async {
        SignupOpenStubURLProtocol.answer(status: 500, body: "oops")
        let open = await client().signupOpen()
        XCTAssertTrue(open)
    }

    func testABodyThatDoesNotParseIsOpen() async {
        SignupOpenStubURLProtocol.answer(status: 200, body: #"{"open":"no"}"#)
        let open = await client().signupOpen()
        XCTAssertTrue(open)
    }

    func testARequestThatCannotBeSentIsOpen() async {
        // An unresolvable base URL: the request never reaches a server.
        let open = await AuthClient(baseURL: "not a url \u{0}", session: Offline.session()).signupOpen()
        XCTAssertTrue(open)
        XCTAssertTrue(SignupCap.isOpen(.failure(URLError(.notConnectedToInternet))))
        XCTAssertFalse(SignupCap.isOpen(.success(false)))
    }

    // MARK: - The bind's refusal

    func testRegistrationClosedIsRecognised() {
        XCTAssertTrue(SignupCap.isRegistrationClosed(AuthClientError.http(
            status: 403, endpoint: "/auth/login-handle", body: #"{"error":"registration-closed"}"#)))
        XCTAssertTrue(SignupCap.isRegistrationClosed(RegistrationClosed()))
    }

    func testOtherRefusalsAreNotRegistrationClosed() {
        // Another 403 code.
        XCTAssertFalse(SignupCap.isRegistrationClosed(AuthClientError.http(
            status: 403, endpoint: "/auth/login-handle", body: #"{"error":"fresh-auth-required"}"#)))
        // The code under another status.
        XCTAssertFalse(SignupCap.isRegistrationClosed(AuthClientError.http(
            status: 409, endpoint: "/auth/login-handle", body: #"{"error":"registration-closed"}"#)))
        // Not JSON.
        XCTAssertFalse(SignupCap.isRegistrationClosed(AuthClientError.http(
            status: 403, endpoint: "/auth/login-handle", body: "registration-closed")))
        XCTAssertFalse(SignupCap.isRegistrationClosed(UsernameTaken(username: "alice")))
    }

    // MARK: - Decisions

    func testTheSignupScreenMode() {
        XCTAssertEqual(SignupScreenMode.after(open: true), .form)
        XCTAssertEqual(SignupScreenMode.after(open: false), .closed)
    }

    func testTheGuestOfferVariant() {
        XCTAssertEqual(GuestOfferVariant.after(open: true), .ask)
        XCTAssertEqual(GuestOfferVariant.after(open: false), .closed)
        XCTAssertEqual(GuestOfferVariant.afterClaimFailure(RegistrationClosed(), current: .ask), .closed)
        XCTAssertEqual(
            GuestOfferVariant.afterClaimFailure(UsernameTaken(username: "mira"), current: .ask), .ask)
    }

    /// `docs/ux/call-links.md` 9.2 — a guest whose earlier attempt already
    /// bound a username holds a place and is never asked the closed question.
    func testAGuestHoldingAUsernameIsNeverClosed() {
        XCTAssertEqual(GuestOfferVariant.after(open: false, holdsUsername: true), .ask)
        XCTAssertEqual(GuestOfferVariant.after(open: true, holdsUsername: true), .ask)
        XCTAssertEqual(
            GuestOfferVariant.afterClaimFailure(RegistrationClosed(), current: .ask, holdsUsername: true),
            .ask)
    }

    // MARK: - Refused at the bind during sign-up

    func testARefusedSignupDeletesThenWipes() async {
        var steps: [String] = []
        await SignupRefusedCleanup.run(
            deleteAtAuth: { steps.append("delete") },
            wipeLocal: { steps.append("wipe") })
        XCTAssertEqual(steps, ["delete", "wipe"])
    }

    func testAFailedDeleteStillWipes() async {
        var steps: [String] = []
        await SignupRefusedCleanup.run(
            deleteAtAuth: {
                steps.append("delete")
                throw URLError(.notConnectedToInternet)
            },
            wipeLocal: { steps.append("wipe") })
        XCTAssertEqual(steps, ["delete", "wipe"])
    }
}
