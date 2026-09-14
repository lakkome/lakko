// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0026 — the tunneled SFU signaling socket reaches the same path as the
/// direct one.
final class SfuTunneledTargetTests: XCTestCase {
    func testKeepsThePathPrefix() {
        let target = SfuSignalingClient.tunneledTarget(baseUrl: "wss://test.lakko.me/sfu", jwt: "a.b.c")
        XCTAssertEqual(target, .init(host: "test.lakko.me", port: 443, innerUrl: "ws://test.lakko.me/sfu/v1/connect?token=a.b.c"))
    }

    func testTrailingSlashesAreTrimmed() {
        let target = SfuSignalingClient.tunneledTarget(baseUrl: "wss://test.lakko.me/sfu//", jwt: "t")
        XCTAssertEqual(target?.innerUrl, "ws://test.lakko.me/sfu/v1/connect?token=t")
    }

    func testOriginOnlyBaseHasNoPrefix() {
        let target = SfuSignalingClient.tunneledTarget(baseUrl: "wss://sfu.lakko.me", jwt: "t")
        XCTAssertEqual(target?.innerUrl, "ws://sfu.lakko.me/v1/connect?token=t")
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
