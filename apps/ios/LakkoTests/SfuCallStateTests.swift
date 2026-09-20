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
            SfuCallStateProbe.url(sfuBaseUrl: "wss://test.lakko.me/sfu", callIdHex: id, jwt: "a.b.c")?.absoluteString,
            "https://test.lakko.me/sfu/v1/calls/\(id)?token=a.b.c")
        XCTAssertEqual(
            SfuCallStateProbe.url(sfuBaseUrl: "ws://127.0.0.1:9640/", callIdHex: id, jwt: "t")?.absoluteString,
            "http://127.0.0.1:9640/v1/calls/\(id)?token=t")
    }

    func testTheTokenIsEscaped() {
        let url = SfuCallStateProbe.url(sfuBaseUrl: "wss://h/sfu", callIdHex: String(repeating: "00", count: 16), jwt: "a b&c")
        XCTAssertTrue(url?.absoluteString.contains("token=a%20b%26c") == true)
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
