// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// Sealed invite links (ADR-0028 / ADR-0009, amended 2026-09-28, flag day):
/// `/i/`, `/c/` and `/g/` all read `https://<host>/<k>/<8-char id>#<16-char
/// secret>` (pointer) or `https://<host>/<k>/<base64url sealed>#<secret>`
/// (inline, the transport-failure fallback). The capability is encrypted
/// under the secret, so a missing, wrong-length or wrong fragment is a
/// damaged link on every kind; the retired 43-character id is refused
/// fail-closed, and a `429` from the ledger reads as "try again later", never
/// as "link invalid".
@MainActor
final class InviteLinkShapeTests: XCTestCase {

    private let issuerSeed = Data(repeating: 0x5A, count: 32)
    private let retiredId = Data(repeating: 0xAB, count: 32)

    // MARK: - The widths come from the core

    func testTheCoreWidthsAreTheShortLinkContract() {
        XCTAssertEqual(inviteBlobIdLen(), 6)
        XCTAssertEqual(inviteBlobIdUrlLen(), 8)
        XCTAssertEqual(inviteLinkSecretLen(), 12)
        XCTAssertEqual(inviteLinkSecretUrlLen(), 16)
        XCTAssertEqual(groupInviteLinkSecretLen(), 12)
        XCTAssertEqual(groupInviteLinkSecretUrlLen(), 16)
        XCTAssertEqual(GroupInviteUrl.blobIdLen, 6)
        XCTAssertEqual(CallInviteUrl.blobIdLen, 6)
        XCTAssertEqual(DiscoveryClient.blobIdLength, 6)
        XCTAssertEqual(InviteLink.secretLen, 12)
        XCTAssertEqual(InviteLink.secretUrlLen, 16)
        XCTAssertEqual(GroupInviteLink.linkSecretLen, 12)
        XCTAssertEqual(GroupInviteLink.linkSecretUrlLen, 16)
    }

    // MARK: - Fragment parsing (every kind)

    func testNoFragmentIsNoSecret() {
        XCTAssertEqual(InviteLink.readFragment(nil), .absent)
        XCTAssertEqual(InviteLink.readFragment(""), .absent)
        XCTAssertNil(InviteLink.secret(fromFragment: nil))
        XCTAssertNil(InviteLink.secret(fromFragment: ""))
    }

    func testAFragmentOfTheWrongLengthIsMalformed() {
        XCTAssertEqual(InviteLink.readFragment(String(repeating: "A", count: 15)), .malformed)
        XCTAssertEqual(InviteLink.readFragment(String(repeating: "A", count: 17)), .malformed)
        // The retired 32-byte (43-character) preview key.
        XCTAssertEqual(InviteLink.readFragment(base64UrlEncodeNoPad(Data(count: 32))), .malformed)
        XCTAssertNil(InviteLink.secret(fromFragment: String(repeating: "A", count: 15)))
    }

    func testAFragmentInTheWrongAlphabetIsMalformed() {
        XCTAssertEqual(InviteLink.readFragment("++++++++////////"), .malformed)
        XCTAssertEqual(InviteLink.readFragment("AAAAAAAAAAAAAA=="), .malformed)
        XCTAssertEqual(InviteLink.readFragment("AAAAAAAA AAAAAAA"), .malformed)
        XCTAssertNil(InviteLink.secret(fromFragment: "++++++++////////"))
    }

    func testAValidFragmentIsTheTwelveByteSecret() {
        let secret = Data((0..<12).map { UInt8($0 * 7) })
        let fragment = base64UrlEncodeNoPad(secret)
        XCTAssertEqual(fragment.count, 16)
        XCTAssertEqual(InviteLink.readFragment(fragment), .secret(secret))
        XCTAssertEqual(InviteLink.secret(fromFragment: fragment), secret)
    }

    // MARK: - The link builder

    func testThePointerLinkIsIdThenSecret() {
        let blobId = Data([1, 2, 3, 4, 5, 6])
        let secret = Data(repeating: 0x11, count: 12)
        let link = InviteLink.pointer(
            origin: "https://lakko.me", prefix: "/c/", blobId: blobId, secret: secret)
        XCTAssertEqual(link, "https://lakko.me/c/" + base64UrlEncodeNoPad(blobId)
            + "#" + base64UrlEncodeNoPad(secret))
        XCTAssertEqual(link.count, "https://lakko.me/c/".count + 8 + 1 + 16)
        let (base, fragment) = InviteLink.split(link)
        XCTAssertEqual(base, "https://lakko.me/c/" + base64UrlEncodeNoPad(blobId))
        XCTAssertEqual(InviteLink.secret(fromFragment: fragment), secret)
    }

    func testTheInlineLinkIsTheSealedBlobThenSecret() {
        let sealed = Data(repeating: 0x42, count: 150)
        let secret = Data(repeating: 0x22, count: 12)
        let link = InviteLink.inline(
            origin: "https://lakko.me", prefix: "/i/", sealed: sealed, secret: secret)
        let (base, fragment) = InviteLink.split(link)
        XCTAssertEqual(base, "https://lakko.me/i/" + base64UrlEncodeNoPad(sealed))
        XCTAssertEqual(fragment, base64UrlEncodeNoPad(secret))
        XCTAssertFalse(link.contains("="))
    }

    func testWithSecretReplacesAnExistingFragment() {
        let first = InviteLink.withSecret("https://lakko.me/g/AAAAAAAA", secret: Data(count: 12))
        let second = InviteLink.withSecret(first, secret: Data(repeating: 0xFF, count: 12))
        XCTAssertEqual(second.filter { $0 == "#" }.count, 1)
        XCTAssertEqual(InviteLink.split(second).base, "https://lakko.me/g/AAAAAAAA")
    }

    func testOnlyATransportFailureFallsBackInline() {
        XCTAssertTrue(InviteLink.uploadFallsBackInline(
            DiscoveryClientError.network(endpoint: "/x", underlying: URLError(.timedOut))))
        XCTAssertFalse(InviteLink.uploadFallsBackInline(
            DiscoveryClientError.http(status: 401, endpoint: "/x", body: "")))
        XCTAssertFalse(InviteLink.uploadFallsBackInline(
            DiscoveryClientError.rateLimited(endpoint: "/x")))
    }

    // MARK: - Compose, then open, through the real FFI

    func testAContactLinkRoundTripsThroughTheSeal() throws {
        let identity = try Identity.fromSeed(seed: issuerSeed)
        let mint = try composeInviteCapability(
            ownerSigningSeed: identity.ed25519Seed(),
            ownerAccountId: Data(repeating: 3, count: 16),
            ownerX25519Pubkey: identity.x25519PublicKey(),
            expiryMs: 0, maxUses: 0, bootstrapReferral: false, label: "Team")
        XCTAssertEqual(mint.linkSecret.count, 12)
        XCTAssertEqual(mint.issuerPubkey, identity.signaturePublicKey())

        // Inline: the link alone carries everything.
        let link = InviteLink.inline(
            origin: "https://lakko.me", prefix: "/i/", sealed: mint.sealed, secret: mint.linkSecret)
        let (base, fragment) = InviteLink.split(link)
        let slug = String(base.dropFirst("https://lakko.me/i/".count))
        guard case .inline(let sealed)? = ContactsRepository.parseInviteSlug(slug) else {
            return XCTFail("the inline /i/ slug parses as inline")
        }
        let secret = try XCTUnwrap(InviteLink.secret(fromFragment: fragment))
        let cap = try InviteCapability.openLink(sealed: sealed, linkSecret: secret)
        XCTAssertEqual(cap.label(), "Team")
        XCTAssertEqual(cap.ownerAccountId(), Data(repeating: 3, count: 16))
        // The plaintext wire verifies on its own and is what the joiner tag
        // is computed over; it is not the sealed blob.
        XCTAssertNotEqual(cap.wire(), mint.sealed)
        XCTAssertNoThrow(try InviteCapability.decodeAndVerify(wire: cap.wire()))
        XCTAssertEqual(
            try computeJoinerInviteTag(capabilityBytes: cap.wire(), joinerAccountId: Data(count: 16)).count,
            32)

        // A wrong secret, and a blob of another kind behind the secret, are
        // the one damaged-link refusal.
        var wrong = secret
        wrong[0] ^= 1
        assertDamaged { _ = try InviteCapability.openLink(sealed: sealed, linkSecret: wrong) }
        assertDamaged { _ = try InviteCapability.openLink(sealed: sealed, linkSecret: Data(count: 5)) }
        assertDamaged { _ = try CallInviteCapability.openLink(sealed: sealed, linkSecret: secret) }
        assertDamaged { _ = try GroupInviteCapability.openLink(sealed: sealed, linkSecret: secret) }
    }

    func testACallLinkRoundTripsThroughTheSeal() throws {
        let identity = try Identity.fromSeed(seed: issuerSeed)
        let callId = Data(repeating: 9, count: 16)
        let mint = try composeCallInviteCapability(
            ownerSigningSeed: identity.ed25519Seed(),
            ownerAccountId: Data(repeating: 4, count: 16),
            callId: callId,
            expiryMs: 0,
            maxUses: UInt32.max,
            media: FfiCallInviteMedia(audio: true, video: true),
            oneTimeKeyPackage: try identity.createKeyPackage(),
            label: "")
        let link = InviteLink.inline(
            origin: "https://lakko.me", prefix: "/c/", sealed: mint.sealed, secret: mint.linkSecret)
        let (base, fragment) = InviteLink.split(link)
        let slug = String(base.dropFirst("https://lakko.me/c/".count))
        guard case .inline(let sealed) = try CallInviteUrl.parseCallInviteSlug(slug) else {
            return XCTFail("the inline /c/ slug parses as inline")
        }
        let cap = try CallInviteCapability.openLink(
            sealed: sealed, linkSecret: try XCTUnwrap(InviteLink.secret(fromFragment: fragment)))
        XCTAssertEqual(cap.callId(), callId)
        XCTAssertTrue(cap.media().video)
        assertDamaged {
            _ = try CallInviteCapability.openLink(sealed: sealed, linkSecret: Data(count: 12))
        }
    }

    func testAGroupLinkRoundTripsAndOneSecretOpensBothLayers() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let cap = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret)
        XCTAssertEqual(cap.groupId(), Data(repeating: 7, count: 32))
        XCTAssertEqual(
            JoinGroupLandingViewModel.previewOutcome(of: cap, secret: mint.linkSecret),
            .opened(GroupInviteFacts(groupName: "Hiking club", inviterName: "Anna", memberCount: 5)))
        assertDamaged {
            _ = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: Data(count: 12))
        }
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
        let link = InviteLink.pointer(
            origin: "https://lakko.me", prefix: "/g/", blobId: Data(count: 6), secret: mint.linkSecret)
        XCTAssertEqual(link.count, "https://lakko.me/g/".count + 8 + 1 + 16)
    }

    func testAGroupInlineLinkParsesBackToTheSealedBlob() throws {
        let sealed = try mintWithPreview(groupId: Data(repeating: 7, count: 32)).sealed
        let url = try GroupInviteUrl.composeInlineGroupInviteUrl(
            capabilityBytes: sealed, origin: "https://lakko.me")
        let slug = String(url.dropFirst("https://lakko.me/g/".count))
        XCTAssertEqual(try GroupInviteUrl.parseGroupInviteSlug(slug), .inline(capabilityBytes: sealed))
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

    func testACallLinkWithoutItsSecretIsDamagedBeforeAnyRequest() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let client = GroupInviteDiscoveryURLProtocol.client(scope)
        let joiner = try Identity.fromSeed(seed: Data(repeating: 0x61, count: 32))
        for fragment in [nil, "", String(repeating: "A", count: 15), "++++++++////////"] as [String?] {
            let result = await CallInviteAcceptor.acceptFromSlug(
                slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
                fragment: fragment,
                joinerAccountId: Data(count: 16),
                joinerIdentity: joiner,
                discoveryClient: client)
            XCTAssertEqual(result, .damaged, "fragment \(String(describing: fragment))")
        }
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope).isEmpty)
    }

    func testACallLinkWhoseBlobDoesNotOpenIsDamaged() async throws {
        let identity = try Identity.fromSeed(seed: issuerSeed)
        let mint = try composeCallInviteCapability(
            ownerSigningSeed: identity.ed25519Seed(),
            ownerAccountId: Data(repeating: 4, count: 16),
            callId: Data(repeating: 9, count: 16),
            expiryMs: 0,
            maxUses: UInt32.max,
            media: FfiCallInviteMedia(audio: true, video: false),
            oneTimeKeyPackage: try identity.createKeyPackage(),
            label: "")
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.sealed(mint.sealed))
        let result = await CallInviteAcceptor.acceptFromSlug(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            fragment: base64UrlEncodeNoPad(Data(count: 12)),
            joinerAccountId: Data(count: 16),
            joinerIdentity: try Identity.fromSeed(seed: Data(repeating: 0x61, count: 32)),
            discoveryClient: GroupInviteDiscoveryURLProtocol.client(scope))
        XCTAssertEqual(result, .damaged)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .contains { $0.method == "GET"
                && $0.path == "/discovery/call-invite/\(GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug)" })
    }

    // MARK: - /i/

    func testAContactSlugIsAPointerOrAnInlineBlob() {
        let blobId = Data([1, 2, 3, 4, 5, 6])
        XCTAssertEqual(ContactsRepository.parseInviteSlug(base64UrlEncodeNoPad(blobId)),
                       .pointer(blobId: blobId))
        let sealed = Data(repeating: 0x44, count: 200)
        XCTAssertEqual(ContactsRepository.parseInviteSlug(base64UrlEncodeNoPad(sealed)),
                       .inline(sealed: sealed))
        XCTAssertNil(ContactsRepository.parseInviteSlug(base64UrlEncodeNoPad(retiredId)))
        XCTAssertNil(ContactsRepository.parseInviteSlug("AbCd+/12"))
        XCTAssertNil(ContactsRepository.parseInviteSlug("AbCdEf1"))
        XCTAssertNil(ContactsRepository.parseInviteSlug(""))
    }

    // MARK: - Deep links keep the fragment

    func testEveryKindParksItsFragmentBesideTheSlug() throws {
        let secret = base64UrlEncodeNoPad(Data(repeating: 5, count: 12))
        let intake = InviteIntake()
        intake.handle(try XCTUnwrap(URL(string: "https://lakko.me/i/AbCd-_12#\(secret)")))
        XCTAssertEqual(intake.pendingSlug, "AbCd-_12")
        XCTAssertEqual(intake.pendingFragment, secret)
        intake.handle(try XCTUnwrap(URL(string: "https://lakko.me/c/AbCd-_12#\(secret)")))
        XCTAssertEqual(intake.pendingCallJoinSlug, "AbCd-_12")
        XCTAssertEqual(intake.pendingCallJoinFragment, secret)
        intake.handle(try XCTUnwrap(URL(string: "https://lakko.me/g/AbCd-_12#\(secret)")))
        XCTAssertEqual(intake.pendingGroupJoinSlug, "AbCd-_12")
        XCTAssertEqual(intake.pendingGroupJoinFragment, secret)

        intake.clear()
        intake.clearCallJoin()
        XCTAssertNil(intake.pendingFragment)
        XCTAssertNil(intake.pendingCallJoinFragment)

        // A link routed without its fragment parks none - and is damaged.
        intake.handle(try XCTUnwrap(URL(string: "https://lakko.me/c/AbCd-_12")))
        XCTAssertNil(intake.pendingCallJoinFragment)
        XCTAssertNil(InviteLink.secret(fromFragment: intake.pendingCallJoinFragment))

        // `Open` on a running link call re-parks the secret with the slug.
        intake.parkCallJoin(slug: "AbCd-_12", fragment: secret)
        XCTAssertEqual(intake.pendingCallJoinFragment, secret)
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

    // MARK: - The preview of an opened /g/ capability

    func testTheRightSecretOpensThePreview() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let cap = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret)
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(of: cap, secret: mint.linkSecret),
                       .opened(GroupInviteFacts(groupName: "Hiking club", inviterName: "Anna", memberCount: 5)))
    }

    func testACapabilityWithoutAPreviewShowsNone() throws {
        let mint = try composeGroupInviteCapability(
            issuerSigningSeed: issuerSeed, groupId: Data(repeating: 7, count: 32), expiryMs: 0)
        let cap = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret)
        XCTAssertFalse(cap.hasPreview())
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(of: cap, secret: mint.linkSecret), .none)
    }

    func testAPreviewThatDoesNotOpenUnderTheSecretIsDamaged() throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let cap = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret)
        var wrong = mint.linkSecret
        wrong[0] ^= 0x01
        XCTAssertEqual(JoinGroupLandingViewModel.previewOutcome(of: cap, secret: wrong), .damaged)
    }

    // MARK: - The landing, end to end

    func testTheLandingRefusesAWrongSecretOnAnInlineLink() async throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let vm = try landing(
            slug: base64UrlEncodeNoPad(mint.sealed),
            fragment: String(repeating: "A", count: 16),
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.ok))
        await vm.run()
        XCTAssertEqual(vm.state, JoinGroupLandingViewModel.State.damaged)
        XCTAssertNil(vm.preview)
    }

    func testTheLandingShowsThePreviewUnderTheRightSecret() async throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let vm = try landing(
            slug: base64UrlEncodeNoPad(mint.sealed),
            fragment: base64UrlEncodeNoPad(mint.linkSecret),
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.ok))
        await vm.run()
        XCTAssertEqual(vm.state, JoinGroupLandingViewModel.State.ready)
        XCTAssertEqual(vm.preview?.groupName, "Hiking club")
    }

    /// A malicious ledger serving another link's sealed blob under a live id:
    /// it does not open under this link's secret.
    func testTheLandingFetchesAPointerAndRefusesASubstitute() async throws {
        let original = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let substitute = try mintWithPreview(groupId: Data(repeating: 8, count: 32))
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.sealed(substitute.sealed))
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            fragment: base64UrlEncodeNoPad(original.linkSecret),
            scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, JoinGroupLandingViewModel.State.damaged)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .contains { $0.method == "GET"
                && $0.path == "/discovery/group-invite/\(GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug)" })
    }

    func testTheLandingOpensAFetchedPointerUnderItsOwnSecret() async throws {
        let mint = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            fragment: base64UrlEncodeNoPad(mint.linkSecret),
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.sealed(mint.sealed)))
        await vm.run()
        XCTAssertEqual(vm.state, JoinGroupLandingViewModel.State.ready)
        XCTAssertEqual(vm.preview?.inviterName, "Anna")
    }

    func testTheLandingRefusesAMissingFragmentWithoutARequest() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug, fragment: nil, scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, JoinGroupLandingViewModel.State.damaged)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .filter { $0.path.hasPrefix("/discovery/") }.isEmpty)
    }

    func testTheLandingRefusesTheOldFragmentWithoutARequest() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug,
            fragment: base64UrlEncodeNoPad(Data(count: 32)),
            scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, JoinGroupLandingViewModel.State.damaged)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .filter { $0.path.hasPrefix("/discovery/") }.isEmpty)
    }

    func testTheLandingRefusesTheOldSlugAsInvalidWithoutARequest() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let vm = try landing(slug: base64UrlEncodeNoPad(retiredId), fragment: validFragment, scope: scope)
        await vm.run()
        XCTAssertEqual(vm.state, JoinGroupLandingViewModel.State.invalid)
        XCTAssertTrue(GroupInviteDiscoveryURLProtocol.requests(scope)
            .filter { $0.path.hasPrefix("/discovery/") }.isEmpty)
    }

    func testTheLandingReadsANotFoundPointerAsInvalid() async throws {
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug, fragment: validFragment,
            scope: GroupInviteDiscoveryURLProtocol.freshScope(.status(404)))
        await vm.run()
        XCTAssertEqual(vm.state, JoinGroupLandingViewModel.State.invalid)
    }

    // MARK: - 429 is "try again later", never "invalid"

    func testTheLandingReadsA429AsTryLater() async throws {
        let vm = try landing(
            slug: GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug, fragment: validFragment,
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
        let wire = try openedWire(mintWithPreview(groupId: Data(repeating: 7, count: 32)))
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
        let wire = try openedWire(mintWithPreview(groupId: Data(repeating: 7, count: 32)))
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

    /// The upload carries the sealed blob and the clear fields the server
    /// reads, under the server's own field names, and never the secret.
    func testAContactUploadCarriesTheSealedBlobAndItsClearFields() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let client = GroupInviteDiscoveryURLProtocol.client(scope).withSessionToken("t")
        let identity = try Identity.fromSeed(seed: issuerSeed)
        let mint = try composeInviteCapability(
            ownerSigningSeed: identity.ed25519Seed(),
            ownerAccountId: Data(repeating: 3, count: 16),
            ownerX25519Pubkey: identity.x25519PublicKey(),
            expiryMs: 1_900_000_000_000, maxUses: 3, bootstrapReferral: true, label: "Team")
        _ = try await client.uploadInvite(
            mint: mint, expiryMs: 1_900_000_000_000, maxUses: 3, label: "Team", bootstrapReferral: true)
        let json = try uploadBody(scope, path: "/discovery/invite")
        XCTAssertEqual(Set(json.keys),
                       ["sealed", "issuer_pubkey", "expiry_ms", "max_uses", "label", "bootstrap_referral"])
        XCTAssertEqual(json["sealed"] as? String, mint.sealed.base64EncodedString())
        XCTAssertEqual(json["issuer_pubkey"] as? String, identity.signaturePublicKey().base64EncodedString())
        XCTAssertEqual((json["expiry_ms"] as? NSNumber)?.uint64Value, 1_900_000_000_000)
        XCTAssertEqual((json["max_uses"] as? NSNumber)?.uint32Value, 3)
        XCTAssertEqual(json["label"] as? String, "Team")
        XCTAssertEqual(json["bootstrap_referral"] as? Bool, true)
        try assertNoSecret(scope, secret: mint.linkSecret)
    }

    func testCallAndGroupUploadsCarryNoContactOnlyFields() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let client = GroupInviteDiscoveryURLProtocol.client(scope).withSessionToken("t")
        let identity = try Identity.fromSeed(seed: issuerSeed)
        let call = try composeCallInviteCapability(
            ownerSigningSeed: identity.ed25519Seed(),
            ownerAccountId: Data(repeating: 4, count: 16),
            callId: Data(repeating: 9, count: 16),
            expiryMs: 5, maxUses: UInt32.max,
            media: FfiCallInviteMedia(audio: true, video: true),
            oneTimeKeyPackage: try identity.createKeyPackage(),
            label: "")
        _ = try await client.uploadCallInvite(mint: call, expiryMs: 5, maxUses: UInt32.max)
        let group = try mintWithPreview(groupId: Data(repeating: 7, count: 32))
        _ = try await client.uploadGroupInvite(mint: group, expiryMs: 0, maxUses: UInt32.max)

        let callJson = try uploadBody(scope, path: "/discovery/call-invite")
        XCTAssertEqual(Set(callJson.keys), ["sealed", "issuer_pubkey", "expiry_ms", "max_uses"])
        XCTAssertEqual(callJson["sealed"] as? String, call.sealed.base64EncodedString())
        XCTAssertEqual((callJson["expiry_ms"] as? NSNumber)?.uint64Value, 5)
        let groupJson = try uploadBody(scope, path: "/discovery/group-invite")
        XCTAssertEqual(Set(groupJson.keys), ["sealed", "issuer_pubkey", "expiry_ms", "max_uses"])
        XCTAssertEqual((groupJson["max_uses"] as? NSNumber)?.uint32Value, UInt32.max)
        try assertNoSecret(scope, secret: call.linkSecret)
        try assertNoSecret(scope, secret: group.linkSecret)
    }

    // MARK: - Helpers

    private var validFragment: String { base64UrlEncodeNoPad(Data(repeating: 0x0F, count: 12)) }

    private func mintWithPreview(groupId: Data) throws -> InviteLinkMint {
        try composeGroupInviteCapabilityWithPreview(
            issuerSigningSeed: issuerSeed,
            groupId: groupId,
            expiryMs: 0,
            groupName: "Hiking club",
            inviterName: "Anna",
            memberCount: 5)
    }

    private func openedWire(_ mint: InviteLinkMint) throws -> Data {
        try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret).wire()
    }

    private func landing(
        slug: String, fragment: String?, scope: String
    ) throws -> JoinGroupLandingViewModel {
        let f = try makeGroupInviteRepo(discoveryScope: scope, token: "t")
        addTeardownBlock { try? FileManager.default.removeItem(at: f.outboxDir) }
        return JoinGroupLandingViewModel(
            slug: slug, linkKeyFragment: fragment, repository: f.repo, intake: InviteIntake())
    }

    private func uploadBody(_ scope: String, path: String) throws -> [String: Any] {
        let seen = try XCTUnwrap(GroupInviteDiscoveryURLProtocol.requests(scope)
            .first { $0.method == "POST" && $0.path == path }, "no upload to \(path)")
        let body = try XCTUnwrap(seen.body, "the upload carried no body")
        return try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
    }

    private func assertNoSecret(
        _ scope: String, secret: Data, file: StaticString = #filePath, line: UInt = #line
    ) throws {
        let encodings = [
            base64UrlEncodeNoPad(secret), secret.base64EncodedString(),
            secret.map { String(format: "%02x", $0) }.joined(),
        ]
        for seen in GroupInviteDiscoveryURLProtocol.requests(scope) {
            let text = (seen.body.flatMap { String(data: $0, encoding: .utf8) } ?? "") + seen.path
            for encoded in encodings {
                XCTAssertFalse(text.contains(encoded), "the link secret reached the server",
                               file: file, line: line)
            }
        }
    }

    private func assertDamaged(
        _ body: () throws -> Void, file: StaticString = #filePath, line: UInt = #line
    ) {
        do {
            try body()
            XCTFail("expected a damaged-link refusal", file: file, line: line)
        } catch {
            guard let ffi = error as? FfiError, case .InviteLinkDamaged = ffi else {
                return XCTFail("expected InviteLinkDamaged, got \(error)", file: file, line: line)
            }
        }
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
