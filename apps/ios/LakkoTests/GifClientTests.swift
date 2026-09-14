// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0047 §4 — the GIF proxy client: URL building against both bases,
/// response parsing, the UX spec §5 failure mapping, and the search debounce.
final class GifClientTests: XCTestCase {

    private final class Recorder: @unchecked Sendable {
        var requests: [URLRequest] = []
    }

    private func client(
        base: String = "https://api.lakko.me/gifs",
        token: String? = "tok",
        status: Int = 200,
        headers: [String: String] = [:],
        body: Data = Data(),
        recorder: Recorder = Recorder()
    ) -> GifClient {
        GifClient(baseURL: base, sessionToken: { token }, perform: { request in
            recorder.requests.append(request)
            let response = HTTPURLResponse(
                url: request.url!, // swiftlint:disable:this force_unwrapping
                statusCode: status,
                httpVersion: "HTTP/1.1",
                headerFields: headers
            )!
            return (body, response)
        })
    }

    // MARK: - URLs

    func testTrendingAndSearchURLsAgainstProductionBase() {
        let c = client()
        XCTAssertEqual(c.listURL(query: "", offset: 0)?.absoluteString, "https://api.lakko.me/gifs/gifs/trending?offset=0")
        XCTAssertEqual(c.listURL(query: "  ", offset: -5)?.absoluteString, "https://api.lakko.me/gifs/gifs/trending?offset=0")
        XCTAssertEqual(
            c.listURL(query: " cats & dogs+ ", offset: 25)?.absoluteString,
            "https://api.lakko.me/gifs/gifs/search?q=cats%20%26%20dogs%2B&offset=25"
        )
    }

    func testLocalBaseAndTrailingSlash() {
        let c = client(base: "http://localhost:8091/")
        XCTAssertEqual(c.listURL(query: "hi", offset: 0)?.absoluteString, "http://localhost:8091/gifs/search?q=hi&offset=0")
        XCTAssertEqual(GifClient.defaultBaseURL, "http://localhost:8091")
    }

    func testQueryIsCappedAt100Characters() {
        let long = String(repeating: "a", count: 150)
        let url = client().listURL(query: long, offset: 0)?.absoluteString ?? ""
        XCTAssertTrue(url.contains("q=" + String(repeating: "a", count: 100) + "&"))
    }

    func testOnlyExactProxyMediaPathsAreFollowed() {
        let c = client()
        XCTAssertEqual(c.mediaURL(relative: "/gifs/media/abc123/preview")?.absoluteString, "https://api.lakko.me/gifs/gifs/media/abc123/preview")
        XCTAssertNotNil(c.mediaURL(relative: "/gifs/media/abc123/full"))
        for bad: Any? in [
            "https://media.giphy.com/x.gif",
            "//evil.example/gifs/media/a/full",
            "/gifs/media/../../x/full",
            "/gifs/media/abc/full?u=https://evil",
            "/gifs/media/abc/original",
            "/gifs/media/ab-c/full",
            "/gifs/media//full",
            "/gifs/media/" + String(repeating: "a", count: 65) + "/full",
            nil,
            42,
        ] {
            XCTAssertNil(c.mediaURL(relative: bad), String(describing: bad))
        }
    }

    // MARK: - Parsing

    func testParsePageDropsItemsOutsideTheContract() {
        let json = """
        {"items":[
          {"id":"ok1","preview_url":"/gifs/media/ok1/preview","full_url":"/gifs/media/ok1/full","width":480,"height":270},
          {"id":"far","preview_url":"https://giphy.com/a.gif","full_url":"/gifs/media/far/full","width":1,"height":1},
          {"id":"big","preview_url":"/gifs/media/big/preview","full_url":"/gifs/media/big/full","width":5000,"height":1},
          {"id":"frac","preview_url":"/gifs/media/frac/preview","full_url":"/gifs/media/frac/full","width":1.5,"height":1},
          "nope"
        ],"next_offset":25}
        """
        let page = client().parsePage(Data(json.utf8))
        XCTAssertEqual(page?.items.map(\.id), ["ok1"])
        XCTAssertEqual(page?.items.first?.width, 480)
        XCTAssertEqual(page?.nextOffset, 25)
        XCTAssertNil(client().parsePage(Data("[]".utf8)))
        XCTAssertNil(client().parsePage(Data(#"{"items":[],"next_offset":null}"#.utf8))?.nextOffset)
    }

    // MARK: - Failure mapping

    func testNotConfiguredOnlyFromTheProxysOwnBody() async {
        let configured = await client(status: 503, body: Data(#"{"error":"gif-provider-not-configured"}"#.utf8))
            .fetchPage(query: "", offset: 0)
        XCTAssertEqual(configured, .failure(.notConfigured))
        // A 503 from nginx in front of a stopped service is an outage.
        let outage = await client(status: 503, body: Data("<html>".utf8)).fetchPage(query: "", offset: 0)
        XCTAssertEqual(outage, .failure(.unavailable))
    }

    func testRateLimitedCarriesRetryAfter() async {
        let limited = await client(status: 429, headers: ["Retry-After": "7"], body: Data(#"{"error":"rate-limited"}"#.utf8))
            .fetchPage(query: "cat", offset: 0)
        XCTAssertEqual(limited, .failure(.rateLimited(retryAfterSecs: 7)))
        XCTAssertEqual(GifClient.failure(status: 429, retryAfter: nil, body: Data()), .rateLimited(retryAfterSecs: 1))
        XCTAssertEqual(GifClient.failure(status: 429, retryAfter: "0.2", body: Data()), .rateLimited(retryAfterSecs: 1))
    }

    func testOtherFailures() async {
        let upstream = await client(status: 502).fetchPage(query: "", offset: 0)
        XCTAssertEqual(upstream, .failure(.unavailable))
        XCTAssertEqual(GifClient.failure(status: 401, retryAfter: nil, body: Data()), .unauthorized)
        let noToken = await client(token: nil).fetchPage(query: "", offset: 0)
        XCTAssertEqual(noToken, .failure(.unauthorized))
        let thrown = await GifClient(baseURL: "https://api.lakko.me/gifs", sessionToken: { "t" }, perform: { _ in
            throw URLError(.notConnectedToInternet)
        }).fetchPage(query: "", offset: 0)
        XCTAssertEqual(thrown, .failure(.unavailable))
    }

    func testRequestsCarryTheBearerToken() async {
        let recorder = Recorder()
        _ = await client(body: Data(#"{"items":[]}"#.utf8), recorder: recorder).fetchPage(query: "x", offset: 0)
        XCTAssertEqual(recorder.requests.first?.value(forHTTPHeaderField: "Authorization"), "Bearer tok")
    }

    func testMediaFetchEnforcesTypeSizeAndOrigin() async throws {
        let base = "https://api.lakko.me/gifs"
        let url = try XCTUnwrap(URL(string: base + "/gifs/media/abc/full"))
        let ok = await client(headers: ["Content-Type": "image/gif; charset=binary"], body: Data([1, 2, 3])).fetchMedia(url, maxBytes: 10)
        XCTAssertEqual(ok, .ok(GifMedia(bytes: Data([1, 2, 3]), mime: "image/gif")))
        let html = await client(headers: ["Content-Type": "text/html"], body: Data([1])).fetchMedia(url, maxBytes: 10)
        XCTAssertEqual(html, .failure(.unavailable))
        let tooBig = await client(headers: ["Content-Type": "image/webp"], body: Data(count: 11)).fetchMedia(url, maxBytes: 10)
        XCTAssertEqual(tooBig, .failure(.unavailable))
        let recorder = Recorder()
        let giphy = try XCTUnwrap(URL(string: "https://media.giphy.com/gifs/media/abc/full"))
        let elsewhere = await client(headers: ["Content-Type": "image/gif"], body: Data([1]), recorder: recorder)
            .fetchMedia(giphy, maxBytes: 10)
        XCTAssertEqual(elsewhere, .failure(.unavailable))
        XCTAssertTrue(recorder.requests.isEmpty, "a URL outside the proxy must never be requested")
    }

    // MARK: - Debounce

    @MainActor
    private final class ManualTimer {
        var pending: [(delay: Int, fire: @MainActor () -> Void, cancelled: Bool)] = []

        func make() -> Debouncer.Timer {
            { [unowned self] delay, fire in
                let index = self.pending.count
                self.pending.append((delay, fire, false))
                return { [unowned self] in self.pending[index].cancelled = true }
            }
        }

        func fireAll() {
            let due = pending
            pending.removeAll()
            for entry in due where !entry.cancelled { entry.fire() }
        }
    }

    @MainActor
    func testDebounceRunsOnlyTheLastScheduledSearch() {
        let timer = ManualTimer()
        let debouncer = Debouncer(delayMs: GifClient.searchDebounceMs, timer: timer.make())
        var ran: [String] = []
        for q in ["c", "ca", "cat"] { debouncer.schedule { ran.append(q) } }
        XCTAssertTrue(ran.isEmpty, "nothing is sent while typing")
        XCTAssertEqual(timer.pending.first?.delay, 350)
        timer.fireAll()
        XCTAssertEqual(ran, ["cat"])
    }

    @MainActor
    func testDebounceFlushAndCancel() {
        let timer = ManualTimer()
        let debouncer = Debouncer(delayMs: 350, timer: timer.make())
        var ran: [String] = []
        debouncer.schedule { ran.append("enter") }
        debouncer.flush()
        XCTAssertEqual(ran, ["enter"])
        timer.fireAll()
        XCTAssertEqual(ran, ["enter"], "a flushed search does not run twice")
        debouncer.schedule { ran.append("closed") }
        debouncer.cancel()
        timer.fireAll()
        XCTAssertEqual(ran, ["enter"])
    }
}
