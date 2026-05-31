// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// G3-4-mobile — iOS half of the dockerised SFU end-to-end gate.
///
/// First-pass test: from inside a booted iOS simulator, hit the
/// `/health` endpoint of `lakko-svc-sfu` brought up by the
/// compose stack at `ci/sfu-e2e/docker-compose.yml`. Expects
/// HTTP 200 with body `ok`.
///
/// This proves three things at once:
///   1. The XCTest target builds + runs against the host app.
///   2. The simulator can reach the host's published Docker
///      port through the standard simulator → host loopback
///      (`http://127.0.0.1:7880/` from inside iOS Simulator
///      maps to the Mac host's `7880` per Apple's published
///      semantics).
///   3. The dockerised SFU's HTTP plane responds — a baseline
///      every subsequent live-RTP / WS / JWT test sits above.
///
/// Reads the SFU URL from `LAKKO_SFU_E2E_URL` (xctest env var)
/// so the workflow can point at whatever address the runner's
/// Docker bridge exposes. Default `http://127.0.0.1:7880` is
/// the compose-stack mapping when `colima` or `Docker.app`
/// publishes the port on the host.
///
/// **What this test does NOT do (out of scope):** dual-instance
/// WS welcome round-trip, JWT minting from inside the
/// simulator, ICE / DTLS over a synthetic peer connection. Each
/// of those is its own sub-sprint (G3-4-mobile-welcome /
/// G3-4-mobile-call) layering above this scaffold.
final class SfuHealthTest: XCTestCase {

    /// Default URL — matches the `sfu` service's published port
    /// in `ci/sfu-e2e/docker-compose.yml`. Overrideable via the
    /// `LAKKO_SFU_E2E_URL` env var the CI workflow injects.
    private var sfuBaseURL: URL {
        if let raw = ProcessInfo.processInfo.environment["LAKKO_SFU_E2E_URL"],
           let url = URL(string: raw) {
            return url
        }
        // Fallback for `xcodebuild test` invocations that don't
        // set the env (e.g. a developer running the test from
        // Xcode against a manually-launched compose stack).
        return URL(string: "http://127.0.0.1:7880")!
    }

    func testHealthEndpointReturnsOk() async throws {
        // `URLSession` does the heavy lifting — no third-party
        // HTTP client necessary. iOS App Transport Security
        // permits cleartext to 127.0.0.1 by default for debug
        // builds in 17.0+ (no Info.plist tweak required).
        let healthURL = sfuBaseURL.appendingPathComponent("health")

        let (data, response) = try await URLSession.shared.data(from: healthURL)

        // Status-code gate: 200 OK.
        guard let http = response as? HTTPURLResponse else {
            XCTFail("URLResponse was not HTTPURLResponse")
            return
        }
        XCTAssertEqual(
            http.statusCode,
            200,
            "SFU /health returned status \(http.statusCode)"
        )

        // Body gate: the SFU emits a plain-text `ok\n` for the
        // health endpoint. We accept a trailing newline +
        // whitespace to absorb framing drift; the substring is
        // what matters.
        let body = String(decoding: data, as: UTF8.self)
        XCTAssertTrue(
            body.trimmingCharacters(in: .whitespacesAndNewlines).contains("ok"),
            "SFU /health body did not contain 'ok': \(body)"
        )
    }
}
