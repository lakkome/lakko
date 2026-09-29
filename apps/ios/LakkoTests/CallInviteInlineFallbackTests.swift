// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0028 amendment 2026-09-28 — a `/c/` link falls back to the inline
/// `<origin>/c/<b64url sealed>#<secret>` form on a TRANSPORT failure of the
/// ledger upload only, as on web and Android; any other upload error surfaces.
final class CallInviteInlineFallbackTests: XCTestCase {

    private let origin = "https://example.test"
    /// Longer than the inline floor, like a real sealed call capability.
    private let sealed = Data((0..<600).map { UInt8($0 % 251) })

    func testAnUploadedLinkIsThePointerForm() async throws {
        let blobId = Data([1, 2, 3, 4, 5, 6])
        let (id, base) = try await CallInviteComposer.pointerOrInlineBase(
            sealed: sealed, origin: origin, upload: { blobId })
        XCTAssertEqual(id, blobId)
        XCTAssertEqual(base, "\(origin)/c/\(base64UrlEncodeNoPad(blobId))")
    }

    func testATransportFailureFallsBackToTheInlineForm() async throws {
        for error: Error in [
            DiscoveryClientError.network(endpoint: "/discovery/call-invite", underlying: URLError(.notConnectedToInternet)),
            URLError(.timedOut),
        ] {
            let (id, base) = try await CallInviteComposer.pointerOrInlineBase(
                sealed: sealed, origin: origin, upload: { throw error })
            XCTAssertNil(id, "an inline link has no ledger row")
            XCTAssertEqual(base, "\(origin)/c/\(base64UrlEncodeNoPad(sealed))")
            // The joiner side reads the inline form back as the sealed bytes.
            XCTAssertEqual(
                try CallInviteUrl.parseCallInviteSlug(base64UrlEncodeNoPad(sealed)),
                .inline(capabilityBytes: sealed))
        }
    }

    func testAServerRefusalSurfaces() async {
        for error: Error in [
            DiscoveryClientError.http(status: 400, endpoint: "/discovery/call-invite", body: ""),
            DiscoveryClientError.http(status: 503, endpoint: "/discovery/call-invite", body: ""),
            DiscoveryClientError.rateLimited(endpoint: "/discovery/call-invite"),
        ] {
            do {
                _ = try await CallInviteComposer.pointerOrInlineBase(
                    sealed: sealed, origin: origin, upload: { throw error })
                XCTFail("a refusal must not fall back to the inline link: \(error)")
            } catch {
                XCTAssertFalse(CallInviteComposer.uploadFallsBackInline(error))
            }
        }
    }
}
