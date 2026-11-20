// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0091 phase 1b — a log tag never carries an identifier.
final class ErrorLogTagTests: XCTestCase {

    func testKeepsTheTypesAndTheStatusNeverTheEndpointOrTheBody() {
        let hex = String(repeating: "c14ae3b45bdb6a66", count: 4)
        let ds = DsClientError.http(status: 401, endpoint: "/ds/group/\(hex)/messages", body: "proof required")
        let tag = ErrorLogTag.of(ChatError.wrapped("retire of \(hex) failed", underlying: ds))
        XCTAssertEqual(tag, "ChatError.wrapped>DsClientError.http status=401")
        XCTAssertFalse(tag.contains(hex))
        XCTAssertFalse(tag.contains("proof"))
    }

    func testNamesATypedRefusal() {
        let tag = ErrorLogTag.of(FfiError.MembershipNotOwner(message: "refused in some group"))
        XCTAssertEqual(tag, "FfiError refusal=notOwner")
    }
}
