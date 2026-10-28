// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import XCTest
import LakkoCore
@testable import Lakko

/// `docs/ux/calls.md`, "Every tile carries the person's name" — the rule
/// behind the name on a remote call tile, and the link call's bookkeeping of
/// who is behind each tile.
///
/// The defect these pin: in a link call a person who had set no profile name
/// was shown as `01a0a045`, the first eight characters of the account id the
/// SFU reports. The receiver read only the display name out of the sealed
/// profile and fell back to that pseudonym.

private let keyA = Data(repeating: 0xA1, count: 32)
private let keyB = Data(repeating: 0xB2, count: 32)
private let accountA = Data(repeating: 0x0A, count: 16)
private let accountB = Data(repeating: 0x0B, count: 16)

private final class MemoryPersistence: ProfilePersistence {
    var blobs: [String: Data] = [:]
    func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
    func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
    func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
}

/// A profile directory on throwaway storage whose username lookups answer
/// `resolve`, with verification driven by the test rather than by a render.
@MainActor
private func makeDirectory(
    defaults: UserDefaults,
    resolve: @escaping (String) async throws -> UsernameResolution = { _ in .notRegistered }
) -> (ProfileDirectory, UsernameVerifier) {
    let verifier = UsernameVerifier(resolve: resolve, defaults: defaults)
    let directory = ProfileDirectory(
        received: ReceivedProfileStore(
            persistence: MemoryPersistence(),
            isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
        verifier: verifier,
        ownStore: OwnProfileStore(defaults: defaults)
    )
    directory.verifiesOnResolve = false
    directory.setRenames(from: [])
    directory.activate(accountIdB64: base64Encode(Data(repeating: 0x01, count: 16)), username: "viewer")
    return (directory, verifier)
}

private func contact(_ key: Data, renamed name: String) -> AcceptedContact {
    AcceptedContact(
        request: IncomingContactRequest(
            messageId: 1, fromPubkey: key, fromAccountId: nil,
            fromPseudonym: Data(count: 32), validAfterMs: 0, intro: nil, createdAt: ""),
        displayName: name)
}

@MainActor
private func remote(_ peerId: String, _ name: String) -> CallUiState.Participant {
    CallUiState.Participant(
        id: SfuCallUi.participantId(forPeer: peerId), displayName: name, isLocal: false,
        micOn: true, camOn: false, handRaised: false, quality: .good,
        speaking: false, screenSharing: false)
}

@MainActor
final class CallTileNameTests: XCTestCase {

    private var suite = ""
    private var defaults: UserDefaults!

    override func setUp() async throws {
        suite = "lakko.tests.call-tile-name.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suite)
    }

    // MARK: - The precedence

    func testWhatTheOneResolverCallsThePersonComesFirst() {
        XCTAssertEqual(
            CallTileName.name(resolved: "Anna (work)", announcedName: "Anna", claimedUsername: "anna"),
            "Anna (work)")
    }

    func testTheNameAnnouncedInTheCallComesNext() {
        XCTAssertEqual(
            CallTileName.name(resolved: nil, announcedName: "Guest", claimedUsername: "guest_1"),
            "Guest")
        XCTAssertEqual(
            CallTileName.name(resolved: "  ", announcedName: " Guest ", claimedUsername: nil),
            "Guest")
    }

    /// The defect's own case: no profile name, a username. Plain text, no `@`.
    func testAClaimedUsernameNamesThePersonWithoutAt() {
        XCTAssertEqual(
            CallTileName.name(resolved: nil, announcedName: nil, claimedUsername: "valentin"),
            "valentin")
    }

    func testUnnamedOnlyWhenNothingHasArrived() {
        XCTAssertNil(CallTileName.name(resolved: nil, announcedName: nil, claimedUsername: nil))
        XCTAssertNil(CallTileName.name(resolved: "  ", announcedName: "", claimedUsername: " \n"))
    }

    // MARK: - Over the one resolver

    /// `@` is for a verified username only; until then the same word is shown
    /// as the plain text the person could equally have typed as a name.
    func testAnUnverifiedUsernameIsPlainAndAVerifiedOneCarriesAt() async {
        let (directory, verifier) = makeDirectory(defaults: defaults) { _ in .signatureKey(keyA) }
        directory.acceptReceived(
            ProfileClaim(displayName: nil, username: "valentin", updatedAtMs: 5),
            signatureKeyB64: base64Encode(keyA), accountIdB64: directory.accountIdB64)
        XCTAssertNil(
            CallTileName.personName(directory.resolve(signatureKey: keyA)),
            "not verified yet: the resolver has no name to give")
        XCTAssertEqual(CallTileName.storedName(signatureKey: keyA, directory: directory), "valentin")

        await verifier.verify(signatureKeyB64: base64Encode(keyA), username: "valentin")
        XCTAssertEqual(CallTileName.storedName(signatureKey: keyA, directory: directory), "@valentin")
    }

    /// A username that failed to verify is never shown with `@`.
    func testAUsernameThatDidNotVerifyStaysPlain() async {
        let (directory, verifier) = makeDirectory(defaults: defaults) { _ in .signatureKey(keyB) }
        directory.acceptReceived(
            ProfileClaim(displayName: nil, username: "pavel", updatedAtMs: 5),
            signatureKeyB64: base64Encode(keyA), accountIdB64: directory.accountIdB64)
        await verifier.verify(signatureKeyB64: base64Encode(keyA), username: "pavel")
        XCTAssertEqual(CallTileName.storedName(signatureKey: keyA, directory: directory), "pavel")
    }

    func testTheViewersRenameComesBeforeTheProfileName() {
        let (directory, _) = makeDirectory(defaults: defaults)
        directory.acceptReceived(
            ProfileClaim(displayName: "Elena", username: nil, updatedAtMs: 5),
            signatureKeyB64: base64Encode(keyA), accountIdB64: directory.accountIdB64)
        XCTAssertEqual(CallTileName.storedName(signatureKey: keyA, directory: directory), "Elena")
        directory.setRenames(from: [contact(keyA, renamed: "Mum")])
        XCTAssertEqual(CallTileName.storedName(signatureKey: keyA, directory: directory), "Mum")
    }

    /// A person nothing is known about has no name: not the key fingerprint,
    /// not the resolver's placeholder word.
    func testAFingerprintOrAPlaceholderIsNeverAName() {
        let (directory, _) = makeDirectory(defaults: defaults)
        let resolved = directory.resolve(signatureKey: keyA)
        XCTAssertTrue(resolved.isPlaceholder)
        XCTAssertNil(CallTileName.personName(resolved))
        XCTAssertNil(CallTileName.storedName(signatureKey: keyA, directory: directory))
    }

    // MARK: - The unnamed state

    /// The unnamed state is the tile's own placeholder: one word, no number,
    /// nothing that could be read as an identifier.
    func testTheUnnamedStateCarriesNoNumber() {
        let label = CallPeerName.tileLabel(displayName: "", isLocal: false)
        XCTAssertFalse(label.isEmpty)
        XCTAssertNil(label.rangeOfCharacter(from: .decimalDigits))
        XCTAssertNotEqual(label, Localization.text("call.participant.numbered", texts: ["number": "1"]))
    }

    // MARK: - Whose profile claim is kept

    /// ADR-0078 section 2: the SFU knows no account, so the one account id a
    /// link call knows independently is the owner's, in the signed
    /// capability. Only a profile the owner sealed that names that account
    /// keeps its account claim and username certificate.
    func testOnlyTheOwnersClaimOfTheOwnersAccountIsKept() {
        let cert = Data(repeating: 0x5C, count: 153)
        let claim = ProfileClaim(
            displayName: "Anna", username: "anna", updatedAtMs: 5,
            accountId: accountA, usernameCertificate: cert)

        let kept = CallTileName.boundProfileClaim(claim, fromOwner: true, ownerAccountId: accountA)
        XCTAssertEqual(kept.accountId, accountA)
        XCTAssertEqual(kept.usernameCertificate, cert)

        for stripped in [
            CallTileName.boundProfileClaim(claim, fromOwner: false, ownerAccountId: accountA),
            CallTileName.boundProfileClaim(claim, fromOwner: true, ownerAccountId: accountB),
        ] {
            XCTAssertNil(stripped.accountId)
            XCTAssertNil(stripped.usernameCertificate)
            XCTAssertEqual(stripped.displayName, "Anna", "the announced name stays")
            XCTAssertEqual(stripped.username, "anna", "the username stays, as a claim")
            XCTAssertEqual(stripped.updatedAtMs, 5)
        }
    }

    // MARK: - Early relays

    func testEarlyRelaysAreHandedBackOldestFirstOnce() {
        var relays = EarlyRelays<Int>()
        relays.keep(1)
        relays.keep(2)
        XCTAssertEqual(relays.drain(), [1, 2])
        XCTAssertEqual(relays.drain(), [])
    }

    func testEarlyRelaysKeepTheNewestSixtyFour() {
        XCTAssertEqual(EarlyRelays<Int>.limit, 64)
        var relays = EarlyRelays<Int>()
        for i in 0..<(64 + 5) { relays.keep(i) }
        let kept = relays.drain()
        XCTAssertEqual(kept.count, 64)
        XCTAssertEqual(kept.first, 5)
        XCTAssertEqual(kept.last, 68)
    }

    // MARK: - The sender side

    /// An account with no display name still announces something to name its
    /// tile by: the username travels in the call envelope whether or not this
    /// device holds its certificate.
    func testTheCallEnvelopeCarriesTheUsernameWhenThereIsNoDisplayName() throws {
        let (directory, _) = makeDirectory(defaults: defaults)
        XCTAssertNil(directory.ownProfile?.displayName)
        let decoded = try decodeProfileEnvelope(plaintext: XCTUnwrap(directory.ownProfileEnvelopeForCall()))
        XCTAssertNil(decoded.displayName)
        XCTAssertEqual(decoded.username, "viewer")
        XCTAssertEqual(decoded.accountId, Data(repeating: 0x01, count: 16))
    }
}

/// The link call's per-peer naming, over the production ports (the one
/// resolver, the profile store). Who a peer is comes from its verified
/// announcement (ADR-0078 section 2), given here as `peerIdentified`.
@MainActor
final class LinkCallTileNamesTests: XCTestCase {

    private var suite = ""
    private var defaults: UserDefaults!

    override func setUp() async throws {
        suite = "lakko.tests.link-call-tile-names.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suite)
    }

    private func names(_ directory: ProfileDirectory) -> LinkCallTileNames {
        LinkCallTileNames(ports: .live(directory: directory))
    }

    /// The defect itself: a sealed profile WITHOUT a display name names the
    /// tile by the username, plain at once and with `@` once the store has
    /// verified it. Never by an identifier.
    func testAProfileWithoutADisplayNameNamesTheTileByTheUsername() async {
        let (directory, verifier) = makeDirectory(defaults: defaults) { _ in .signatureKey(keyA) }
        let names = names(directory)
        let ui = CallUiState()
        let first = names.name(peerId: "p1")
        XCTAssertEqual(first, "", "nothing has arrived from this person yet")
        ui.setParticipants([remote("p1", first)])

        names.peerIdentified("p1", person: base64Encode(keyA), memberKeyHex: "aa")
        names.memberProfile(
            from: "p1", sender: base64Encode(keyA), senderLeafKeyHex: "aa",
            fromOwner: true, ownerAccountId: accountA,
            profile: ProfileClaim(
                displayName: nil, username: "valentin", updatedAtMs: 5,
                accountId: accountA, usernameCertificate: Data(repeating: 0x5C, count: 153)))
        names.relabel(ui)
        XCTAssertEqual(ui.participants.first?.displayName, "valentin")
        XCTAssertEqual(
            directory.storedProfile(signatureKeyB64: base64Encode(keyA))?.username, "valentin",
            "filed under the person who sealed it, like a profile received in a chat")

        await verifier.verify(signatureKeyB64: base64Encode(keyA), username: "valentin")
        names.relabel(ui)
        XCTAssertEqual(ui.participants.first?.displayName, "@valentin")
    }

    /// The viewer's own name for a contact applies to the person a peer was
    /// verified as.
    func testAnIdentifiedPersonIsNamedByTheOneResolver() {
        let (directory, _) = makeDirectory(defaults: defaults)
        let names = names(directory)
        _ = names.name(peerId: "p1")
        names.peerIdentified("p1", person: base64Encode(keyA), memberKeyHex: "aa")
        names.memberProfile(
            from: "p1", sender: base64Encode(keyA), senderLeafKeyHex: "aa",
            fromOwner: false, ownerAccountId: accountB,
            profile: ProfileClaim(displayName: "Elena", username: nil, updatedAtMs: 5, accountId: accountA))
        XCTAssertEqual(names.name(peerId: "p1"), "Elena")
        directory.setRenames(from: [contact(keyA, renamed: "Mum")])
        XCTAssertEqual(names.name(peerId: "p1"), "Mum")
    }

    /// A profile that is not the owner's own keeps no account claim: it is
    /// filed without one, and a username it carries is shown as plain text,
    /// never filed where it could look verified.
    func testAProfileFromAnotherMemberKeepsNoAccountClaim() {
        let (directory, _) = makeDirectory(defaults: defaults)
        let names = names(directory)
        _ = names.name(peerId: "p1")
        names.peerIdentified("p1", person: base64Encode(keyA), memberKeyHex: "aa")
        names.memberProfile(
            from: "p1", sender: base64Encode(keyA), senderLeafKeyHex: "aa",
            fromOwner: false, ownerAccountId: accountA,
            profile: ProfileClaim(
                displayName: "Anna", username: "anna", updatedAtMs: 5,
                accountId: accountA, usernameCertificate: Data(repeating: 0x5C, count: 153)))
        XCTAssertNil(directory.storedProfile(signatureKeyB64: base64Encode(keyA)), "not filed")
        XCTAssertEqual(names.name(peerId: "p1"), "Anna")

        names.memberProfile(
            from: "p1", sender: base64Encode(keyA), senderLeafKeyHex: "aa",
            fromOwner: false, ownerAccountId: accountA,
            profile: ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 6, accountId: accountA))
        let stored = directory.storedProfile(signatureKeyB64: base64Encode(keyA))
        XCTAssertEqual(stored?.displayName, "Anna")
        XCTAssertNil(stored?.accountId, "filed without the unbound account claim")
    }

    /// A profile relayed by a peer other than the one that sealed it names
    /// nobody's tile: the relaying peer announced a different leaf.
    func testARelayedProfileNamesNobody() {
        let (directory, _) = makeDirectory(defaults: defaults)
        let names = names(directory)
        _ = names.name(peerId: "p1")
        names.peerIdentified("p1", person: base64Encode(keyA), memberKeyHex: "aa")
        names.memberProfile(
            from: "p1", sender: base64Encode(keyB), senderLeafKeyHex: "bb",
            fromOwner: false, ownerAccountId: accountA,
            profile: ProfileClaim(displayName: "Boris", username: nil, updatedAtMs: 5))
        XCTAssertEqual(names.name(peerId: "p1"), "", "Boris's profile does not name p1")
    }

    /// A sender the group could not resolve is nobody to file a profile under.
    func testAProfileWithoutAResolvedSenderIsNotFiled() {
        let (directory, _) = makeDirectory(defaults: defaults)
        let names = names(directory)
        _ = names.name(peerId: "p1")
        names.peerIdentified("p1", person: base64Encode(keyA), memberKeyHex: "aa")
        names.memberProfile(
            from: "p1", sender: nil, senderLeafKeyHex: "aa",
            fromOwner: false, ownerAccountId: accountA,
            profile: ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 5, accountId: accountA))
        XCTAssertTrue(directory.receivedProfiles().isEmpty)
        XCTAssertEqual(names.name(peerId: "p1"), "Anna")
    }

    /// A username announced without its certificate names the tile but stays
    /// out of the store, where it could never verify and would stand in the
    /// way of the certified profile that follows it.
    func testAUsernameWithoutACertificateIsShownButNotFiled() {
        let (directory, _) = makeDirectory(defaults: defaults)
        let names = names(directory)
        _ = names.name(peerId: "p1")
        names.peerIdentified("p1", person: base64Encode(keyA), memberKeyHex: "aa")
        names.memberProfile(
            from: "p1", sender: base64Encode(keyA), senderLeafKeyHex: "aa",
            fromOwner: true, ownerAccountId: accountA,
            profile: ProfileClaim(displayName: nil, username: "valentin", updatedAtMs: 5, accountId: accountA))
        XCTAssertNil(directory.storedProfile(signatureKeyB64: base64Encode(keyA)))
        XCTAssertEqual(names.name(peerId: "p1"), "valentin")
    }

    /// A person this device already knows is named as soon as their peer's
    /// announcement verifies, and not before.
    func testAKnownPersonIsNamedOnceIdentified() {
        let (directory, _) = makeDirectory(defaults: defaults)
        directory.acceptReceived(
            ProfileClaim(displayName: "Boris", username: nil, updatedAtMs: 3, accountId: accountB),
            signatureKeyB64: base64Encode(keyB), accountIdB64: directory.accountIdB64)
        let names = names(directory)
        XCTAssertEqual(names.name(peerId: "p9"), "")
        names.peerIdentified("p9", person: base64Encode(keyB), memberKeyHex: "bb")
        XCTAssertEqual(names.name(peerId: "p9"), "Boris")
    }

    /// Two people nothing has arrived from are both unnamed: an empty name,
    /// no number, and nothing of the peer id.
    func testTheUnnamedAreNotNumbered() {
        let (directory, _) = makeDirectory(defaults: defaults)
        let names = names(directory)
        XCTAssertEqual(names.name(peerId: "01a0a045-0000"), "")
        XCTAssertEqual(names.name(peerId: "01a0a046-0000"), "")
    }

    /// A peer that leaves takes what it announced with it.
    func testAPeerThatLeftStartsUnnamedAgain() {
        let (directory, _) = makeDirectory(defaults: defaults)
        let names = names(directory)
        _ = names.name(peerId: "p1")
        names.peerIdentified("p1", person: base64Encode(keyB), memberKeyHex: "bb")
        names.memberProfile(
            from: "p1", sender: nil, senderLeafKeyHex: "bb",
            fromOwner: false, ownerAccountId: accountA,
            profile: ProfileClaim(displayName: "Guest", username: nil, updatedAtMs: 1))
        XCTAssertEqual(names.name(peerId: "p1"), "Guest")
        names.peerLeft("p1")
        XCTAssertEqual(names.name(peerId: "p1"), "")
    }

    /// The tiles are named again when the profile store changes, without a
    /// roster event: a username verification finishing, a contact renamed.
    func testAStoreChangeRenamesTheTilesOnScreen() async {
        let changes = PassthroughSubject<UInt64, Never>()
        var known: [MemberIdentity: String] = [:]
        let names = LinkCallTileNames(ports: LinkCallTileNames.Ports(
            personName: { known[$0] },
            changes: changes.eraseToAnyPublisher()))
        let ui = CallUiState()
        ui.setParticipants([remote("p1", names.name(peerId: "p1"))])
        names.peerIdentified("p1", person: base64Encode(keyA), memberKeyHex: "aa")
        names.follow(ui: ui)
        XCTAssertEqual(ui.participants.first?.displayName, "")

        // The first value is the one the publisher holds at subscription.
        changes.send(0)
        known[base64Encode(keyA)] = "@valentin"
        changes.send(1)
        for _ in 0..<200 where ui.participants.first?.displayName != "@valentin" {
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertEqual(ui.participants.first?.displayName, "@valentin")
    }
}

/// ADR-0078 section 2 - the SFU group call's tile names: unnamed until a
/// peer's announcement verifies, then the person's name, following the store.
@MainActor
final class GroupCallPeerNamesTests: XCTestCase {

    func testATileIsUnnamedUntilItsPeerIsIdentified() {
        let names = GroupCallPeerNames(name: { $0 == base64Encode(keyA) ? "Anna" : nil })
        names.setRoster(["p1", "p2"])
        XCTAssertEqual(names.name(participantId: "peer-p1"), "")
        XCTAssertEqual(names.identify("p1", personKeyB64: base64Encode(keyA)), "Anna")
        XCTAssertEqual(names.name(participantId: "peer-p1"), "Anna")
        XCTAssertNil(
            names.identify("p1", personKeyB64: base64Encode(keyA)),
            "a re-announcement of the same person changes nothing")
        XCTAssertEqual(
            names.identify("p2", personKeyB64: base64Encode(keyB)), "",
            "a person this device cannot name stays unnamed")
    }

    func testAPeerNotOnTheRosterIsNotNamed() {
        let names = GroupCallPeerNames(name: { _ in "Anna" })
        names.setRoster([])
        XCTAssertNil(names.identify("ghost", personKeyB64: base64Encode(keyA)))
        names.peerJoined("p3")
        XCTAssertEqual(names.identify("p3", personKeyB64: base64Encode(keyA)), "Anna")
        names.peerLeft("p3")
        XCTAssertNil(names.identify("p3", personKeyB64: base64Encode(keyA)))
    }

    func testRelabelAppliesTheCurrentNames() {
        var known: [String: String] = [:]
        let names = GroupCallPeerNames(name: { known[$0] })
        let ui = CallUiState()
        names.setRoster(["p1"])
        ui.setParticipants([remote("p1", "")])
        _ = names.identify("p1", personKeyB64: base64Encode(keyA))
        known[base64Encode(keyA)] = "Mum"
        names.relabel(ui)
        XCTAssertEqual(ui.participants.first?.displayName, "Mum")
    }
}
