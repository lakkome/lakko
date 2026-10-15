// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 4.3 — asking the SFU whether a call is still
/// there. The same URL and the same reading of the answer as web and Android.
final class SfuCallStateTests: XCTestCase {
    func testTheStateRouteIsOnTheHttpOriginBehindTheSocketBase() {
        let id = String(repeating: "ab", count: 16)
        XCTAssertEqual(
            SfuCallStateProbe.url(sfuBaseUrl: "wss://test.lakko.me/sfu", callIdHex: id)?.absoluteString,
            "https://test.lakko.me/sfu/v1/calls/\(id)")
        XCTAssertEqual(
            SfuCallStateProbe.url(sfuBaseUrl: "ws://127.0.0.1:9640/", callIdHex: id)?.absoluteString,
            "http://127.0.0.1:9640/v1/calls/\(id)")
    }

    /// The token rides `Authorization`, never the URL (invariant #5).
    func testTheTokenIsABearerHeaderNotAQuery() {
        let id = String(repeating: "00", count: 16)
        let request = SfuCallStateProbe.request(sfuBaseUrl: "wss://h/sfu", callIdHex: id, jwt: "a.b.c")
        XCTAssertEqual(request?.url?.absoluteString, "https://h/sfu/v1/calls/\(id)")
        XCTAssertNil(request?.url?.query)
        XCTAssertEqual(request?.value(forHTTPHeaderField: "Authorization"), "Bearer a.b.c")
    }

    func testTheFourWordsAreRead() {
        for word in ["unknown", "live", "closing", "ended"] {
            let data = Data("{\"state\":\"\(word)\",\"peers\":0}".utf8)
            XCTAssertEqual(SfuCallStateProbe.parse(data)?.rawValue, word)
        }
    }

    /// Anything else is no answer, never `ended`: a probe that cannot be read
    /// must never end a live call.
    func testAnythingElseIsNoAnswer() {
        XCTAssertNil(SfuCallStateProbe.parse(Data("{\"state\":\"gone\"}".utf8)))
        XCTAssertNil(SfuCallStateProbe.parse(Data("{}".utf8)))
        XCTAssertNil(SfuCallStateProbe.parse(Data("<html>".utf8)))
    }

    func testTheProbeIsSlowerThanTheSfuGrace() {
        XCTAssertEqual(SfuCallStateProbe.intervalMs, 10_000)
    }
}
