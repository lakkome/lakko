// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// A `/c/` link is always the ledger pointer `<origin>/c/<id>#<secret>`. The
/// inline form was removed on 2026-10-19: any failed upload, a network one
/// included, mints no link and surfaces, as on web and Android.
final class CallInviteUploadFailureTests: XCTestCase {

    private let origin = "https://example.test"

    func testAnUploadedLinkIsThePointerForm() async throws {
        let blobId = Data([1, 2, 3, 4, 5, 6])
        let (id, base) = try await CallInviteComposer.pointerBase(origin: origin, upload: { blobId })
        XCTAssertEqual(id, blobId)
        XCTAssertEqual(base, "\(origin)/c/\(base64UrlEncodeNoPad(blobId))")
    }

    func testEveryUploadFailureMintsNothing() async {
        for error: Error in [
            DiscoveryClientError.network(
                endpoint: "/discovery/call-invite", underlying: URLError(.notConnectedToInternet)),
            URLError(.timedOut),
            DiscoveryClientError.http(status: 400, endpoint: "/discovery/call-invite", body: ""),
            DiscoveryClientError.http(status: 503, endpoint: "/discovery/call-invite", body: ""),
            DiscoveryClientError.rateLimited(endpoint: "/discovery/call-invite"),
        ] {
            do {
                let minted = try await CallInviteComposer.pointerBase(origin: origin, upload: { throw error })
                XCTFail("a failed upload must mint no link, got \(minted.base)")
            } catch {}
        }
    }
}
