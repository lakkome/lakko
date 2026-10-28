// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/calls.md` section 4.3 — asking the SFU whether a call is still
/// there. The same URL and the same reading of the answer as web and Android.
final class SfuCallStateTests: XCTestCase {
    func testTheStateRouteIsOnTheHttpOriginBehindTheSocketBase() {
        let room = String(repeating: "ab", count: 32)
        XCTAssertEqual(
            SfuCallStateProbe.url(sfuBaseUrl: "wss://test.lakko.me/sfu", roomHex: room)?.absoluteString,
            "https://test.lakko.me/sfu/v1/rooms/\(room)")
        XCTAssertEqual(
            SfuCallStateProbe.url(sfuBaseUrl: "ws://127.0.0.1:9640/", roomHex: room)?.absoluteString,
            "http://127.0.0.1:9640/v1/rooms/\(room)")
    }

    /// ADR-0078 section 2: the probe carries no token at all.
    func testTheProbeCarriesNoToken() {
        let room = String(repeating: "00", count: 32)
        let request = SfuCallStateProbe.request(sfuBaseUrl: "wss://h/sfu", roomHex: room)
        XCTAssertEqual(request?.url?.absoluteString, "https://h/sfu/v1/rooms/\(room)")
        XCTAssertNil(request?.url?.query)
        XCTAssertNil(request?.value(forHTTPHeaderField: "Authorization"))
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
