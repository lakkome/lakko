// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0026 — the tunneled SFU signaling socket reaches the same path as the
/// direct one, with the admission token in the offered subprotocols.
final class SfuTunneledTargetTests: XCTestCase {
    func testKeepsThePathPrefix() {
        let target = SfuSignalingClient.tunneledTarget(baseUrl: "wss://test.lakko.me/sfu", jwt: "a.b.c")
        XCTAssertEqual(target, .init(
            host: "test.lakko.me",
            port: 443,
            innerUrl: "ws://test.lakko.me/sfu/v1/connect",
            subprotocols: "lakko-sfu-v1,lakko-sfu-token.a.b.c"))
    }

    func testTrailingSlashesAreTrimmed() {
        let target = SfuSignalingClient.tunneledTarget(baseUrl: "wss://test.lakko.me/sfu//", jwt: "t")
        XCTAssertEqual(target?.innerUrl, "ws://test.lakko.me/sfu/v1/connect")
    }

    func testOriginOnlyBaseHasNoPrefix() {
        let target = SfuSignalingClient.tunneledTarget(baseUrl: "wss://sfu.lakko.me", jwt: "t")
        XCTAssertEqual(target?.innerUrl, "ws://sfu.lakko.me/v1/connect")
    }

    /// The admission token rides `Sec-WebSocket-Protocol` beside
    /// `lakko-sfu-v1`, never the URL (invariant #5); the direct path offers
    /// the same two subprotocols.
    func testTheTokenIsASubprotocolNotAQuery() {
        let target = SfuSignalingClient.tunneledTarget(baseUrl: "wss://sfu.lakko.me", jwt: "x.y.z")
        XCTAssertFalse(target?.innerUrl.contains("x.y.z") ?? true)
        XCTAssertEqual(target?.subprotocols, "lakko-sfu-v1,lakko-sfu-token.x.y.z")
        XCTAssertEqual(sfuSubprotocols(jwt: "x.y.z"), ["lakko-sfu-v1", "lakko-sfu-token.x.y.z"])
    }

    func testExplicitPortAndPlainScheme() {
        let target = SfuSignalingClient.tunneledTarget(baseUrl: "ws://10.0.0.2:7880", jwt: "t")
        XCTAssertEqual(target?.host, "10.0.0.2")
        XCTAssertEqual(target?.port, 7880)
    }

    func testNoHostIsRefused() {
        XCTAssertNil(SfuSignalingClient.tunneledTarget(baseUrl: "not a url", jwt: "t"))
    }
}
