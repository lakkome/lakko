// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// Short invite links (ADR-0028 amendment of 2026-09-27, flag day):
/// `/i/<8>`, `/c/<8>`, `/g/<8>#<16>`. The 8 characters are a 6-byte ledger id
/// the server mints, the 16 the 12-byte group-invite link secret. The old
/// 43-character id and fragment are refused fail-closed, a `/g/` link whose
/// fragment does not open the preview is refused as damaged, and a `429` from
/// the ledger reads as "try again later", never as "link invalid".
@MainActor
final class InviteLinkShapeTests: XCTestCase {

    private let issuerSeed = Data(repeating: 0x5A, count: 32)
    private let retiredId = Data(repeating: 0xAB, count: 32)

    // MARK: - The widths come from the core

    func testTheCoreWidthsAreTheShortLinkContract() {
        XCTAssertEqual(inviteBlobIdLen(), 6)
        XCTAssertEqual(inviteBlobIdUrlLen(), 8)
        XCTAssertEqual(groupInviteLinkSecretLen(), 12)
        XCTAssertEqual(groupInviteLinkSecretUrlLen(), 16)
        XCTAssertEqual(GroupInviteUrl.blobIdLen, 6)
        XCTAssertEqual(CallInviteUrl.blobIdLen, 6)
        XCTAssertEqual(DiscoveryClient.blobIdLength, 6)
        XCTAssertEqual(GroupInviteLink.linkSecretLen, 12)
        XCTAssertEqual(GroupInviteLink.linkSecretUrlLen, 16)
    }

    // MARK: - /g/

    func testAGroupPointerLinkIsEightCharactersAndParsesBack() throws {
        let blobId = Data([0x01, 0x02, 0xFE, 0xFF, 0x10, 0x20])
        let url = try GroupInviteUrl.composePointerGroupInviteUrl(
            blobId: blobId, origin: "https://lakko.me")
        let slug = String(url.dropFirst("https://lakko.me/g/".count))
        XCTAssertEqual(slug.count, 8)
        XCTAssertEqual(try GroupInviteUrl.parseGroupInviteSlug(slug), .pointer(blobId: blobId))
    }

    func testAWholePointerLinkWithItsSecretIsAboutFortyFiveCharacters() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        XCTAssertEqual(mint.linkKey.count, 12)
        let base = try GroupInviteUrl.composePointerGroupInviteUrl(
            blobId: Data(count: 6), origin: "https://lakko.me")
        let link = GroupInviteLink.withLinkKey(base, linkKey: mint.linkKey)
        XCTAssertEqual(link.count, "https://lakko.me/g/".count + 8 + 1 + 16)
    }

    func testAGroupInlineLinkParsesBackToTheCapability() throws {
        let wire = try mintWithPreview(groupId: Data(repeating: 7, count: 32)).wire
        let url = try GroupInviteUrl.composeInlineGroupInviteUrl(
            capabilityBytes: wire, origin: "https://lakko.me")
        let slug = String(url.dropFirst("https://lakko.me/g/".count))
        XCTAssertEqual(try GroupInviteUrl.parseGroupInviteSlug(slug), .inline(capabilityBytes: wire))
    }

    func testAGroupSlugOfAnyOtherShapeIsRefused() {
        // The retired 32-byte (43-character) content-addressed id.
        XCTAssertThrowsError(try GroupInviteUrl.parseGroupInviteSlug(base64UrlEncodeNoPad(retiredId)))
        XCTAssertThrowsError(try GroupInviteUrl.composePointerGroupInviteUrl(blobId: retiredId))
        // Between the two forms.
        XCTAssertThrowsError(try GroupInviteUrl.parseGroupInviteSlug(
            base64UrlEncodeNoPad(Data(count: 7))))
        XCTAssertThrowsError(try GroupInviteUrl.parseGroupInviteSlug(
            base64UrlEncodeNoPad(Data(count: 99))))
        XCTAssertThrowsError(try GroupInviteUrl.parseGroupInviteSlug(
            base64UrlEncodeNoPad(Data(count: 5))))
        // Eight characters in the standard alphabet, or padded: not a link.
        XCTAssertEqual(
            try? GroupInviteUrl.parseGroupInviteSlug("AbCd+/12"), nil)
        XCTAssertEqual(
            try? GroupInviteUrl.parseGroupInviteSlug("AbCdEf=="), nil)
        XCTAssertEqual(try? GroupInviteUrl.parseGroupInviteSlug(""), nil)
    }

    // MARK: - /c/

    func testACallPointerLinkIsEightCharactersAndParsesBack() throws {
        let blobId = Data([9, 8, 7, 6, 5, 4])
        let url = try CallInviteUrl.composePointerCallInviteUrl(
            blobId: blobId, origin: "https://lakko.me")
        let slug = String(url.dropFirst("https://lakko.me/c/".count))
        XCTAssertEqual(slug.count, 8)
        XCTAssertEqual(try CallInviteUrl.parseCallInviteSlug(slug), .pointer(blobId: blobId))
    }

    func testACallInlineLinkParsesBack() throws {
        let bytes = Data(repeating: 0x33, count: CallInviteUrl.inlineMinLen)
        let url = try CallInviteUrl.composeInlineCallInviteUrl(
            capabilityBytes: bytes, origin: "https://lakko.me")
        let slug = String(url.dropFirst("https://lakko.me/c/".count))
        XCTAssertEqual(try CallInviteUrl.parseCallInviteSlug(slug), .inline(capabilityBytes: bytes))
    }

    func testACallSlugOfTheRetiredWidthIsRefused() {
        XCTAssertThrowsError(try CallInviteUrl.parseCallInviteSlug(base64UrlEncodeNoPad(retiredId)))
        XCTAssertThrowsError(try CallInviteUrl.composePointerCallInviteUrl(blobId: retiredId))
        XCTAssertThrowsError(try CallInviteUrl.parseCallInviteSlug("AbCd+/12"))
    }

    // MARK: - /i/

    func testAContactSlugIsExactlyEightCharactersOfBase64Url() {
        let blobId = Data([1, 2, 3, 4, 5, 6])
        XCTAssertEqual(ContactsRepository.parseInviteSlug(base64UrlEncodeNoPad(blobId)), blobId)
        XCTAssertNil(ContactsRepository.parseInviteSlug(base64UrlEncodeNoPad(retiredId)))
        XCTAssertNil(ContactsRepository.parseInviteSlug("AbCd+/12"))
        XCTAssertNil(ContactsRepository.parseInviteSlug("AbCdEf1"))
        XCTAssertNil(ContactsRepository.parseInviteSlug(""))
    }

    // MARK: - Revocations sign the 6-byte id

    func testTheRevocationSignersTakeTheSixByteId() throws {
        let id = Data(count: 6)
        XCTAssertFalse(try signInviteRevocation(signingSeed: issuerSeed, blobId: id, signedAtMs: 1).isEmpty)
        XCTAssertFalse(try signCallInviteRevocation(signingSeed: issuerSeed, blobId: id, signedAtMs: 1).isEmpty)
        XCTAssertFalse(try signGroupInviteRevocation(
            issuerSigningSeed: issuerSeed, blobId: id, signedAtMs: 1).isEmpty)
        XCTAssertThrowsError(try signGroupInviteRevocation(
            issuerSigningSeed: issuerSeed, blobId: retiredId, signedAtMs: 1))
    }

    // MARK: - The preview decides whether a /g/ link stands

    func testTheRightSecretOpensThePreview() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let cap = try GroupInviteCapability.decodeAndVerify(wire: mint.wire)
        let outcome = JoinGroupLandingViewModel.previewOutcome(
            of: cap, fragment: base64UrlEncodeNoPad(mint.linkKey))
        XCTAssertEqual(outcome, .opened(GroupInviteFacts(
            groupName: "Hiking club", inviterName: "Anna", memberCount: 5)))
    }

    func testNoFragmentIsACardWithoutAPreview() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let cap = try GroupInviteCapability.decodeAndVerify(wire: mint.wire)
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(of: cap, fragment: nil), .none)
    }

    func testAWrongSecretRefusesTheLinkAsDamaged() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let cap = try GroupInviteCapability.decodeAndVerify(wire: mint.wire)
        var wrong = mint.linkKey
        wrong[0] ^= 0x01
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(
            of: cap, fragment: base64UrlEncodeNoPad(wrong)), .damaged)
    }

    func testTheRetiredFortyThreeCharacterFragmentIsDamaged() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let cap = try GroupInviteCapability.decodeAndVerify(wire: mint.wire)
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(
            of: cap, fragment: base64UrlEncodeNoPad(Data(count: 32))), .damaged)
    }

    /// A malicious ledger serving another validly signed capability under a
    /// live id: the preview key binds the group id and issuer, so the link's
    /// secret opens nothing on the substitute.
    func testASubstitutedCapabilityDoesNotOpenUnderTheLinksSecret() throws {
        let original = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let substitute = try mintWithPreview(groupId: Data(repeating: 8, count: 32))
        let cap = try GroupInviteCapability.decodeAndVerify(wire: substitute.wire)
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(
            of: cap, fragment: base64UrlEncodeNoPad(original.linkKey)), .damaged)
    }

    func testACapabilityWithoutAPreviewUnderAFragmentIsDamaged() throws {
        let wire = try composeGroupInviteCapability(
            issuerSigningSeed: issuerSeed, groupId: Data(repeating: 7, count: 32), expiryMs: 0)
        let cap = try GroupInviteCapability.decodeAndVerify(wire: wire)
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(
            of: cap, fragment: String(repeating: "A", count: 16)), .damaged)
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(of: cap, fragment: nil), .none)
    }

    // MARK: - The landing, end to end

    func testTheLandingRefusesAWrongSecretOnAnInlineLink() async throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let vm = try landing(
            slug: base64UrlEncodeNoPad(mint.wire),
            fragment: String(repeating: "A", count: 16),
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.ok))
        await vm.run()
        XCTAssertEqual(vm.state, .damaged)
        XCTAssertNil(vm.preview)
    }

    func testTheLandingShowsThePreviewUnderTheRightSecret() async throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let vm = try landing(
            slug: base64UrlEncodeNoPad(mint.wire),
            fragment: base64UrlEncodeNoPad(mint.linkKey),
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.ok))
        await vm.run()
        XCTAssertEqual(vm.state, .ready)
        XCTAssertEqual(vm.preview?.groupName, "Hiking club")
    }

    func testTheLandingFetchesAPointerAndRefusesASubstitute() async throws {
        let original = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let substitute = try mintWithPreview(groupId: Data(repeating: 8, count: 32))
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.capability(substitute.wire))
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            fragment: base64UrlEncodeNoPad(original.linkKey),
            scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, .damaged)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .contains { $0.method == "GET"
                && $0.path == "/discovery/group-invite/\(GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug)" })
    }

    func testTheLandingOpensAFetchedPointerUnderItsOwnSecret() async throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            fragment: base64UrlEncodeNoPad(mint.linkKey),
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.capability(mint.wire)))
        await vm.run()
        XCTAssertEqual(vm.state, .ready)
        XCTAssertEqual(vm.preview?.inviterName, "Anna")
    }

    func testTheLandingRefusesTheOldFragmentWithoutARequest() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            fragment: base64UrlEncodeNoPad(Data(count: 32)),
            scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, .damaged)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .filter { $0.path.hasPrefix("/discovery/") }.isEmpty)
    }

    func testTheLandingRefusesTheOldSlugAsInvalidWithoutARequest() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let vm = try landing(slug: base64UrlEncodeNoPad(retiredId), fragment: nil, scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, .invalid)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .filter { $0.path.hasPrefix("/discovery/") }.isEmpty)
    }

    func testTheLandingReadsANotFoundPointerAsInvalid() async throws {
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug, fragment: nil,
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.status(404)))
        await vm.run()
        XCTAssertEqual(vm.state, .invalid)
    }

    // MARK: - 429 is "try again later", never "invalid"

    func testTheLandingReadsA429AsTryLater() async throws {
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug, fragment: nil,
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.status(429)))
        await vm.run()
        XCTAssertEqual(vm.state, .failed(.tryLater))
        XCTAssertTrue(JoinRequestFailure.tryLater.offersRetry)
        XCTAssertEqual(JoinRequestFailure.tryLater.message, inviteRateLimitedMessage)
        XCTAssertNotEqual(inviteRateLimitedMessage, groupInviteInvalidMessage)
    }

    func testEveryLedgerFetchAndConsumeSurfacesA429AsRateLimited() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.status(429))
        let client = GroupInviteDiscoveryURLProtocol.client(scope)
        let id = Data(count: 6)
        let wire = try mintWithPreview(groupId: Data(repeating: 7, count: 32)).wire
        let joiner = Data(repeating: 3, count: 16)

        await assertRateLimited { _ = try await client.fetchInvite(blobId: id) }
        await assertRateLimited { _ = try await client.fetchCallInvite(blobId: id) }
        await assertRateLimited { _ = try await client.fetchGroupInvite(blobId: id) }
        await assertRateLimited {
            _ = try await client.consumeInvite(blobId: id, capabilityBytes: wire, joinerAccountId: joiner)
        }
        await assertRateLimited {
            _ = try await client.consumeCallInvite(blobId: id, capabilityBytes: wire, joinerAccountId: joiner)
        }
        await assertRateLimited {
            _ = try await client.consumeGroupInvite(blobId: id, capabilityBytes: wire, joinerAccountId: joiner)
        }
    }

    func testA404IsNotReadAsARateLimit() async throws {
        let client = GroupInviteDiscoveryURLProtocol.client(
            GroupInviteDiscoveryURLProtocol.freshScope(.status(404)))
        let fetched = try await client.fetchGroupInvite(blobId: Data(count: 6))
        XCTAssertNil(fetched)
    }

    func testTheRateLimitIsFoundThroughTheRepositoryWrappers() {
        let limited = DiscoveryClientError.rateLimited(endpoint: "/x")
        XCTAssertTrue(DiscoveryClientError.isRateLimited(limited))
        XCTAssertTrue(DiscoveryClientError.isRateLimited(ChatError.wrapped("x", underlying: limited)))
        XCTAssertTrue(DiscoveryClientError.isRateLimited(ContactsError.wrapped(limited)))
        XCTAssertFalse(DiscoveryClientError.isRateLimited(
            DiscoveryClientError.http(status: 404, endpoint: "/x", body: "")))
        XCTAssertEqual(
            JoinRequestFailure.classify(ChatError.wrapped("x", underlying: limited)), .tryLater)
        XCTAssertEqual(ChatRepository.discoveryFailureSummary(limited), "rate limited")
    }

    func testARequestToJoinRefusedAt429IsTryLater() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.status(429))
        let f = try makeGroupInviteRepo(discoveryScope: scope, token: "t")
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }
        let wire = try mintWithPreview(groupId: Data(repeating: 7, count: 32)).wire
        do {
            _ = try await f.repo.requestToJoinFromLink(
                capabilityWire: wire,
                nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
                blobId: Data(count: 6))
            XCTFail("expected the 429 to surface")
        } catch {
            XCTAssertEqual(JoinRequestFailure.classify(error), .tryLater)
        }
    }

    // MARK: - Uploads

    func testAnUploadAnswerInTheRetiredWidthIsRefused() async throws {
        let client = GroupInviteDiscoveryURLProtocol.client(
            GroupInviteDiscoveryURLProtocol.freshScope(.uploadAnswers(base64UrlEncodeNoPad(retiredId))))
            .withSessionToken("t")
        let wire = try mintWithPreview(groupId: Data(repeating: 7, count: 32)).wire
        do {
            let id = try await client.uploadGroupInvite(capabilityBytes: wire, maxUses: .max)
            XCTFail("a 32-byte id must be refused, got \(id.count) bytes")
        } catch {}
    }

    func testAnUploadReturnsTheSixByteId() async throws {
        let client = GroupInviteDiscoveryURLProtocol.client(
            GroupInviteDiscoveryURLProtocol.freshScope(.ok)).withSessionToken("t")
        let wire = try mintWithPreview(groupId: Data(repeating: 7, count: 32)).wire
        let id = try await client.uploadGroupInvite(capabilityBytes: wire, maxUses: .max)
        XCTAssertEqual(id.count, 6)
        XCTAssertEqual(base64UrlEncodeNoPad(id), GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug)
    }

    // MARK: - Helpers

    private func mintWithPreview(groupId: Data) throws -> GroupInviteMint {
        try composeGroupInviteCapabilityWithPreview(
            issuerSigningSeed: issuerSeed,
            groupId: groupId,
            expiryMs: 0,
            groupName: "Hiking club",
            inviterName: "Anna",
            memberCount: 5)
    }

    private func landing(
        slug: String, fragment: String?, scope: String
    ) throws -> JoinGroupLandingViewModel {
        let f = try makeGroupInviteRepo(discoveryScope: scope, token: "t")
        addTeardownBlock { try? FileManager.default.removeItem(at: f.outboxDir) }
        return JoinGroupLandingViewModel(
            slug: slug, linkKeyFragment: fragment, repository: f.repo, intake: InviteIntake())
    }

    private func assertRateLimited(
        _ body: () async throws -> Void, file: StaticString = #filePath, line: UInt = #line
    ) async {
        do {
            try await body()
            XCTFail("expected a rate-limit refusal", file: file, line: line)
        } catch {
            guard let discovery = error as? DiscoveryClientError,
                  case .rateLimited = discovery else {
                return XCTFail("expected rateLimited, got \(error)", file: file, line: line)
            }
        }
    }
}
