// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// Short sealed invite links (ADR-0028 amendments of 2026-09-27 and
/// 2026-09-28, flag days): `/i/<8>#<16>`, `/c/<8>#<16>`, `/g/<8>#<16>`. The 8
/// characters are a 6-byte ledger id the server mints, the 16 the 12-byte link
/// secret the capability is sealed under. The old 43-character id is refused
/// fail-closed, a link whose fragment is missing or does not open the blob is
/// refused as damaged, and a `429` from the ledger reads as "try again later",
/// never as "link invalid".
@MainActor
final class InviteLinkShapeTests: XCTestCase {
    /// ADR-0058 section 6 — every `/i/` and `/c/` capability carries a link
    /// id and its `link` grant; these tests only need a well-formed pair.
    static let testLinkId = Data(repeating: 0x4C, count: Int(linkIdLen()))
    static var testLinkGrant: FfiReachGrant {
        // swiftlint:disable:next force_try
        try! deriveReachGrant(
            accountSeed: Data(repeating: 5, count: 32), class: .link,
            scopeId: testLinkId, version: 1)
    }


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
        XCTAssertEqual(inviteLinkSecretLen(), 12)
        XCTAssertEqual(inviteLinkSecretUrlLen(), 16)
        XCTAssertEqual(InviteLinkSecret.secretLen, 12)
        XCTAssertEqual(InviteLinkSecret.urlLen, 16)
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
        XCTAssertEqual(mint.linkSecret.count, 12)
        let base = try GroupInviteUrl.composePointerGroupInviteUrl(
            blobId: Data(count: 6), origin: "https://lakko.me")
        let link = InviteLinkSecret.link(base: base, secret: mint.linkSecret)
        XCTAssertEqual(link.count, "https://lakko.me/g/".count + 8 + 1 + 16)
    }

    func testAGroupInlineLinkParsesBackToTheSealedBlob() throws {
        let wire = try mintWithPreview(groupId: Data(repeating: 7, count: 32)).sealed
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

    // MARK: - The link secret decides whether a /g/ link stands

    private let anySecretShape = String(repeating: "A", count: 16)

    func testTheRightSecretOpensTheCapabilityAndThePreview() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let cap = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret)
        XCTAssertEqual(cap.groupId(), Data(repeating: 7, count: 32))
        XCTAssertEqual(
            JoinGroupLandingViewModel.previewOutcome(of: cap, secret: mint.linkSecret),
            .opened(GroupInviteFacts(groupName: "Hiking club", inviterName: "Anna", memberCount: 5)))
        // The plaintext is what the joiner tag and the attribution use.
        XCTAssertEqual(try GroupInviteCapability.decodeAndVerify(wire: cap.wire()).groupId(), cap.groupId())
    }

    func testAWrongSecretDoesNotOpenAndIsDamaged() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        var wrong = mint.linkSecret
        wrong[0] ^= 0x01
        XCTAssertThrowsError(try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: wrong)) {
            XCTAssertTrue(InviteLinkSecret.isDamaged($0), "\($0)")
        }
    }

    func testAMissingOrMalformedFragmentIsDamaged() {
        XCTAssertNil(InviteLinkSecret.read(nil))
        XCTAssertNil(InviteLinkSecret.read(""))
        XCTAssertNil(InviteLinkSecret.read(base64UrlEncodeNoPad(Data(count: 32))), "the retired width")
        XCTAssertNil(InviteLinkSecret.read("AbCd+/12AbCd+/12"), "not base64url")
        XCTAssertEqual(InviteLinkSecret.read(anySecretShape)?.count, 12)
    }

    /// A malicious ledger serving another link's sealed blob under a live id:
    /// the link's secret opens nothing on the substitute.
    func testASubstitutedBlobDoesNotOpenUnderTheLinksSecret() throws {
        let original = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let substitute = try mintWithPreview(groupId: Data(repeating: 8, count: 32))
        XCTAssertThrowsError(try GroupInviteCapability.openLink(
            sealed: substitute.sealed, linkSecret: original.linkSecret))
    }

    /// A blob of another kind behind `/g/` does not open either.
    func testACallBlobDoesNotOpenAsAGroupLink() throws {
        let call = try composeCallInviteCapability(
            ownerSigningSeed: issuerSeed, ownerAccountId: Data(repeating: 1, count: 16),
            callId: Data(repeating: 2, count: 16), expiryMs: 0, maxUses: 0,
            media: FfiCallInviteMedia(audio: true, video: false),
            oneTimeKeyPackage: try Identity.fromSeed(seed: Data(repeating: 9, count: 32)).createKeyPackage(),
            label: "",
            linkId: Self.testLinkId, linkGrant: Self.testLinkGrant)
        XCTAssertThrowsError(try GroupInviteCapability.openLink(sealed: call.sealed, linkSecret: call.linkSecret))
        XCTAssertNoThrow(try CallInviteCapability.openLink(sealed: call.sealed, linkSecret: call.linkSecret))
    }

    /// A capability minted without a preview (a co-publisher link) is a card
    /// without one; the secret already opened the blob.
    func testACapabilityWithoutAPreviewShowsNone() throws {
        let mint = try composeGroupInviteCapability(
            issuerSigningSeed: issuerSeed, groupId: Data(repeating: 7, count: 32), expiryMs: 0)
        let cap = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret)
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(of: cap, secret: mint.linkSecret), .none)
    }

    // MARK: - The landing, end to end

    func testTheLandingRefusesAWrongSecretOnAnInlineLink() async throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let vm = try landing(
            slug: base64UrlEncodeNoPad(mint.sealed),
            fragment: anySecretShape,
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.ok))
        await vm.run()
        XCTAssertEqual(vm.state, .damaged)
        XCTAssertNil(vm.preview)
    }

    func testTheLandingShowsThePreviewUnderTheRightSecret() async throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let vm = try landing(
            slug: base64UrlEncodeNoPad(mint.sealed),
            fragment: base64UrlEncodeNoPad(mint.linkSecret),
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.ok))
        await vm.run()
        XCTAssertEqual(vm.state, .ready)
        XCTAssertEqual(vm.preview?.groupName, "Hiking club")
    }

    func testTheLandingRefusesALinkWithNoFragmentWithoutARequest() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug, fragment: nil, scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, .damaged)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .filter { $0.path.hasPrefix("/discovery/") }.isEmpty)
    }

    func testTheLandingFetchesAPointerAndRefusesASubstitute() async throws {
        let original = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let substitute = try mintWithPreview(groupId: Data(repeating: 8, count: 32))
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.capability(substitute.sealed))
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            fragment: base64UrlEncodeNoPad(original.linkSecret),
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
            fragment: base64UrlEncodeNoPad(mint.linkSecret),
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.capability(mint.sealed)))
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
        let vm = try landing(slug: base64UrlEncodeNoPad(retiredId), fragment: anySecretShape, scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, .invalid)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .filter { $0.path.hasPrefix("/discovery/") }.isEmpty)
    }

    func testTheLandingReadsANotFoundPointerAsInvalid() async throws {
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug, fragment: anySecretShape,
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.status(404)))
        await vm.run()
        XCTAssertEqual(vm.state, .invalid)
    }

    // MARK: - 429 is "try again later", never "invalid"

    func testTheLandingReadsA429AsTryLater() async throws {
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug, fragment: anySecretShape,
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
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let wire = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret).wire()
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
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let wire = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret).wire()
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
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        do {
            let id = try await client.uploadGroupInvite(mint: mint, expiryMs: 0, maxUses: .max)
            XCTFail("a 32-byte id must be refused, got \(id.count) bytes")
        } catch {}
    }

    func testAnUploadReturnsTheSixByteId() async throws {
        let client = GroupInviteDiscoveryURLProtocol.client(
            GroupInviteDiscoveryURLProtocol.freshScope(.ok)).withSessionToken("t")
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let id = try await client.uploadGroupInvite(mint: mint, expiryMs: 0, maxUses: .max)
        XCTAssertEqual(id.count, 6)
        XCTAssertEqual(base64UrlEncodeNoPad(id), GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug)
    }

    // MARK: - Sealed uploads and opens for /i/ and /c/

    /// The ledger gets the SEALED blob and clear fields only; `/i/` adds its
    /// label and referral flag, `/c/` and `/g/` do not.
    func testTheUploadBodiesCarryTheSealedBlobAndTheClearFields() throws {
        let contact = try JSONSerialization.jsonObject(with: JSONEncoder().encode(InviteUploadRequest(
            sealed: "U0VBTEVE", issuerPubkey: "S0VZ", expiryMs: 7, maxUses: 0,
            label: "Team", bootstrapReferral: true))) as? [String: Any]
        XCTAssertEqual(Set(contact?.keys.map { $0 } ?? []),
                       ["sealed", "issuer_pubkey", "expiry_ms", "max_uses", "label", "bootstrap_referral"])
        let group = try JSONSerialization.jsonObject(with: JSONEncoder().encode(InviteUploadRequest(
            sealed: "U0VBTEVE", issuerPubkey: "S0VZ", expiryMs: 0, maxUses: .max))) as? [String: Any]
        XCTAssertEqual(Set(group?.keys.map { $0 } ?? []), ["sealed", "issuer_pubkey", "expiry_ms", "max_uses"])
        XCTAssertNil(group?["capability"], "the plaintext capability never reaches the ledger")
    }

    func testAContactLinkOpensUnderItsSecretOnly() throws {
        let owner = try Identity.fromSeed(seed: issuerSeed)
        let mint = try composeInviteCapability(
            ownerSigningSeed: owner.ed25519Seed(), ownerAccountId: Data(repeating: 4, count: 16),
            ownerX25519Pubkey: owner.x25519PublicKey(), expiryMs: 0, maxUses: 0,
            bootstrapReferral: false, label: "",
            linkId: Self.testLinkId, linkGrant: Self.testLinkGrant)
        XCTAssertEqual(mint.issuerPubkey, owner.signaturePublicKey())
        let cap = try InviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret)
        XCTAssertEqual(cap.ownerAccountId(), Data(repeating: 4, count: 16))
        var wrong = mint.linkSecret
        wrong[3] ^= 0x10
        XCTAssertThrowsError(try InviteCapability.openLink(sealed: mint.sealed, linkSecret: wrong)) {
            XCTAssertTrue(InviteLinkSecret.isDamaged($0))
        }
    }

    /// A tapped `/i/` or `/c/` link parks its fragment with the slug; the
    /// secret never becomes part of what the ledger is asked for.
    func testTheIntakeKeepsTheFragmentOfContactAndCallLinks() throws {
        let intake = InviteIntake()
        intake.handle(try XCTUnwrap(URL(string: "https://lakko.me/i/AbCd-_12#AAAAAAAAAAAAAAAA")))
        XCTAssertEqual(intake.pendingSlug, "AbCd-_12#AAAAAAAAAAAAAAAA")
        let parts = InviteLinkSecret.split(try XCTUnwrap(intake.pendingSlug))
        XCTAssertEqual(ContactsRepository.parseInviteSlug(parts.base)?.count, 6)
        XCTAssertEqual(InviteLinkSecret.read(parts.fragment)?.count, 12)
        intake.handle(try XCTUnwrap(URL(string: "https://lakko.me/c/AbCd-_12")))
        XCTAssertEqual(intake.pendingCallJoinSlug, "AbCd-_12", "no fragment: parked as is, damaged on open")
    }

    /// A `/c/` link with no secret is damaged before anything is fetched.
    func testACallLinkWithoutItsSecretIsDamaged() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let result = await CallInviteAcceptor.acceptFromSlug(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            joinerAccountId: Data(repeating: 1, count: 16),
            joinerIdentity: try Identity.fromSeed(seed: Data(repeating: 2, count: 32)),
            discoveryClient: GroupInviteDiscoveryURLProtocol.client(scope))
        XCTAssertEqual(result, .damaged)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope).isEmpty)
        XCTAssertEqual(inviteLinkDamagedMessage, "This invite link is damaged. Ask for a new link.")
    }

    /// A contact link with no secret is damaged before anything is fetched.
    func testTheSharedDamagedCopyIsTheGroupPagesCopy() {
        XCTAssertEqual(inviteLinkDamagedMessage, groupInviteDamagedMessage)
    }

    // MARK: - Helpers

    private func mintWithPreview(groupId: Data) throws -> InviteLinkMint {
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
