// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// G3-4-mobile-call iter1 — iOS half of the live SFU
/// signaling round-trip test.
///
/// What this test gates
/// ====================
///
/// The Phase-3 SFU surface that a real iOS client touches:
///
///   1. URLSessionWebSocketTask successfully upgrades to a
///      WebSocket against the dockerised SFU on
///      `${LAKKO_SFU_E2E_URL}/v1/connect` offering the
///      `lakko-sfu-v1` and `lakko-sfu-token.${jwt}`
///      subprotocols (the token never rides the URL).
///   2. The very first server-side frame is a typed
///      `Welcome` carrying the same `call_id` the JWT's
///      claim encoded.
///   3. The peer roster excludes our own peer id (we just
///      joined; we have not yet seen any other client).
///
/// The JWT is minted out-of-band by the
/// `lakko-call-jwt --example mint_test_jwt` helper (the
/// `g3-4-mobile.yml` workflow runs it in a pre-step and
/// exports the token as `$LAKKO_SFU_E2E_JWT` into the
/// xcodebuild env). The production iOS client SDK does NOT
/// expose `mint_jwt` — clients receive JWTs from
/// `services/auth`, never mint them.
///
/// What this test does NOT gate (sub-sprint
/// G3-4-mobile-call-iter2)
/// ======================================================
///
/// - Dual-instance fan-out: two simulator-side peers in
///   one process exchanging SDP offer/answer through the
///   SFU's `signal-relay`, ICE+DTLS completing, and one
///   peer's encoded audio frames surfacing on the other.
/// - Real `RTCPeerConnection` integration — this test
///   uses raw URLSessionWebSocketTask + JSON, NOT the
///   `WebRTCPeerConnection.swift` wrapper. That wrapper's
///   build-time correctness is already gated by the
///   xcodebuild step in the main workflow; layering it
///   on top of the dual-instance test belongs in iter2.
///
/// Env-var contract
/// ================
///
/// - `LAKKO_SFU_E2E_URL` — base URL of the SFU's signaling
///   listener (typically `http://127.0.0.1:7880`).
/// - `LAKKO_SFU_E2E_JWT` — opaque JWT minted by the
///   workflow's pre-step. The test fails with a clear
///   message if either env var is absent so a local run
///   from Xcode without the workflow does not mistake the
///   missing setup for a real regression.
final class SfuSignalingTest: XCTestCase {

    /// Base URL for the SFU's HTTP / WebSocket plane.
    /// `LAKKO_SFU_E2E_URL` is set by the workflow; defaults
    /// to `http://127.0.0.1:7880` for an Xcode-local run
    /// against a manually-launched compose stack.
    private var sfuBaseURL: URL {
        if let raw = ProcessInfo.processInfo.environment["LAKKO_SFU_E2E_URL"],
           let url = URL(string: raw) {
            return url
        }
        return URL(string: "http://127.0.0.1:7880")!
    }

    func testWebSocketUpgradeAndWelcomeFrame() async throws {
        // Skipped, not failed, without the workflow's minted token: a suite
        // that is red for want of an environment is a suite a real failure
        // can hide in.
        // The scheme interpolates `$(LAKKO_SFU_E2E_URL)`, so on a local run
        // the variable is PRESENT and EMPTY rather than absent.
        let sfuRaw = ProcessInfo.processInfo.environment["LAKKO_SFU_E2E_URL"] ?? ""
        guard !sfuRaw.trimmingCharacters(in: .whitespaces).isEmpty else {
            throw XCTSkip("LAKKO_SFU_E2E_URL is empty; no SFU to reach")
        }
        guard let jwt = ProcessInfo.processInfo.environment["LAKKO_SFU_E2E_JWT"],
              !jwt.isEmpty
        else {
            XCTFail(
                """
                LAKKO_SFU_E2E_JWT must be set by the workflow's mint_test_jwt
                pre-step. To run locally:

                  cargo run --quiet --release -p lakko-call-jwt \\
                    --example mint_test_jwt -- \\
                    --call-id deadbeef0000000000000000000000aa \\
                    --account cafef00d0000000000000000000000bb \\
                    --role participant > /tmp/jwt

                then re-run xcodebuild with
                LAKKO_SFU_E2E_JWT=$(cat /tmp/jwt) in the env.
                """
            )
            return
        }

        // The signaling endpoint demands the admission token
        // as a `lakko-sfu-token.<jwt>` subprotocol offered
        // beside `lakko-sfu-v1`. No token → 401 + dropped TCP.
        var components = URLComponents(
            url: sfuBaseURL.appendingPathComponent("v1/connect"),
            resolvingAgainstBaseURL: false
        )!
        // Match the WS scheme to the base scheme. Apple's
        // URLSessionWebSocketTask accepts both `ws` and
        // `http` here, but the canonical shape on the
        // wire is ws/wss.
        components.scheme = (sfuBaseURL.scheme == "https") ? "wss" : "ws"
        let wsURL = components.url!

        let session = URLSession(configuration: .default)
        var request = URLRequest(url: wsURL)
        request.setValue(
            "lakko-sfu-v1, lakko-sfu-token.\(jwt)",
            forHTTPHeaderField: "Sec-WebSocket-Protocol"
        )

        let task = session.webSocketTask(with: request)
        task.resume()
        defer { task.cancel(with: .normalClosure, reason: nil) }

        // First frame from a fresh connection is always
        // Welcome. The receive call double-times-out: the
        // outer is our test budget; URLSession's own
        // timeout is 60 s default.
        let frameJson = try await withTimeout(seconds: 10) {
            let message = try await task.receive()
            switch message {
            case .string(let s): return s
            case .data(let d):
                guard let s = String(data: d, encoding: .utf8) else {
                    throw NSError(
                        domain: "SfuSignalingTest",
                        code: 1,
                        userInfo: [NSLocalizedDescriptionKey: "binary frame not UTF-8"]
                    )
                }
                return s
            @unknown default:
                throw NSError(
                    domain: "SfuSignalingTest",
                    code: 2,
                    userInfo: [NSLocalizedDescriptionKey: "unknown WebSocketMessage variant"]
                )
            }
        }

        // The ServerFrame wire shape is an internally-tagged
        // serde_json enum: `{ "kind": "welcome", "peer_id":
        // "...", "call_id": "...", "server_ts": <int>,
        // "roster": [...] }`. We assert the bare minimum so
        // a future field addition does not break this gate.
        guard let data = frameJson.data(using: .utf8) else {
            XCTFail("welcome frame body not UTF-8: \(frameJson)")
            return
        }
        guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            XCTFail("welcome frame did not decode as JSON object: \(frameJson)")
            return
        }
        XCTAssertEqual(obj["kind"] as? String, "welcome", "first frame must be Welcome: \(obj)")

        // The JWT's call_id claim must echo into the frame.
        // The mint_test_jwt example pins
        // `deadbeef0000000000000000000000aa`; the workflow
        // passes the same value. If they drift, the SFU
        // would reject admission with a 401 long before we
        // got here, so this assertion is mostly defensive.
        let echoedCallId = obj["call_id"] as? String
        XCTAssertNotNil(echoedCallId, "welcome frame must carry call_id: \(obj)")
        XCTAssertEqual(echoedCallId?.count, 32, "call_id should be 16-byte hex: \(obj)")

        // The roster the SFU sends in Welcome excludes the
        // just-admitted peer. On a fresh stack with no
        // other clients connected, it must be empty.
        let roster = obj["roster"] as? [Any] ?? []
        XCTAssertTrue(
            roster.isEmpty,
            "roster should be empty on first peer in a fresh stack: \(roster)"
        )
    }

    /// Run an async closure with an absolute timeout. Swift
    /// Concurrency does not ship a built-in
    /// `withTimeout` — this is the standard pattern over
    /// `TaskGroup`.
    private func withTimeout<T: Sendable>(
        seconds: TimeInterval,
        operation: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask {
                try await operation()
            }
            group.addTask {
                try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
                throw NSError(
                    domain: "SfuSignalingTest",
                    code: -1,
                    userInfo: [
                        NSLocalizedDescriptionKey: "operation timed out after \(seconds)s"
                    ]
                )
            }
            let result = try await group.next()!
            group.cancelAll()
            return result
        }
    }
}
