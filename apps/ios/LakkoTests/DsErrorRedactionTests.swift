// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// A DS error's message reaches logs, so it must name no journal (ADR-0078
/// section 11); the error case itself keeps the full endpoint.
final class DsErrorRedactionTests: XCTestCase {
    func testAnErrorMessageNamesNoJournal() {
        let id = String(repeating: "ab", count: 32)
        let http = DsClientError.http(status: 401, endpoint: "/ds/group/\(id)/messages?after=17", body: "{}")
        XCTAssertEqual(http.errorDescription, "ds /ds/group/:id/messages → 401: {}")
        let net = DsClientError.network(endpoint: "/ds/group/\(id)/messages", underlying: URLError(.timedOut))
        XCTAssertFalse(net.errorDescription?.contains(id) ?? true)
        XCTAssertEqual(DsClientError.redacted("/ds/mailbox/0123456789abcdef0123"), "/ds/mailbox/:id")
    }
}
