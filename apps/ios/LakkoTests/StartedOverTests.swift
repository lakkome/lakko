// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0101 sections 10 and 11 / UX `identity-recovery.md` 16.11 — what a
/// contact's device keeps about an account's seals and about an UNSEALED
/// reset ("started over"): the announcement floor, the record that closes the
/// old one-to-one, the request from the new keys, and the adoption of those
/// keys when the person accepts it. Plus UX 19.5: saving a phrase changes no
/// key.
@MainActor
final class StartedOverTests: XCTestCase {

    private let account = "YWNjdA=="
    private let old = "b2xkLWtleQ=="
    private let new = "bmV3LWtleQ=="
    private let newer = "bmV3ZXIta2V5"

    private func pins(_ name: String = #function) -> PeerPinStore {
        let suite = "lakko.tests.started-over.\(name)"
        UserDefaults().removePersistentDomain(forName: suite)
        let store = PeerPinStore(defaults: UserDefaults(suiteName: suite)!, ownerAccountIdB64: "b3duZXI=")
        store.setPin(accountIdB64: account, pinnedSignaturePubkeyB64: old, rotationMarker: 10, keyGeneration: 1)
        return store
    }

    // MARK: - The announcement floor

    func testTheFloorOnlyRisesAndOnlyForAPinnedAccount() {
        let store = pins()
        XCTAssertEqual(store.sealSeqFloor(accountIdB64: account), 0)
        XCTAssertTrue(store.raiseSealSeqFloor(accountIdB64: account, seq: 4))
        XCTAssertEqual(store.sealSeqFloor(accountIdB64: account), 4)
        XCTAssertFalse(store.raiseSealSeqFloor(accountIdB64: account, seq: 4), "not higher")
        XCTAssertFalse(store.raiseSealSeqFloor(accountIdB64: account, seq: 2), "an older announcement served again")
        XCTAssertEqual(store.sealSeqFloor(accountIdB64: account), 4)
        XCTAssertFalse(store.raiseSealSeqFloor(accountIdB64: "b3RoZXI=", seq: 9), "no pin, no floor")
        XCTAssertEqual(store.sealSeqFloor(accountIdB64: "b3RoZXI="), 0)
    }

    /// A sealed reset moves the pin and keeps the floor at the announcement
    /// it was sealed under.
    func testASealedResetRepinsAndKeepsTheFloor() {
        let store = pins()
        store.raiseSealSeqFloor(accountIdB64: account, seq: 3)
        let result = store.acceptReset(PeerPinStore.AcceptedReset(
            accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5,
            nowMs: 20_000, announcementSeq: 7))
        XCTAssertEqual(result, .accepted)
        XCTAssertEqual(store.readPin(accountIdB64: account)?.pinnedSignaturePubkeyB64, new)
        XCTAssertGreaterThanOrEqual(store.sealSeqFloor(accountIdB64: account), 7)
        XCTAssertNotNil(store.loadSupersededKeys()[old], "the old key is the same person's, retired")
        XCTAssertTrue(store.loadStartedOverKeys().isEmpty, "a sealed reset closes nothing")
    }

    // MARK: - An unsealed reset

    func testAnUnsealedResetIsRecordedAndThePinStaysOnTheOldKey() {
        let store = pins()
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .recorded)
        // Nothing moved: the pin, the generation.
        XCTAssertEqual(store.readPin(accountIdB64: account)?.pinnedSignaturePubkeyB64, old)
        XCTAssertEqual(store.acceptedGeneration(accountIdB64: account), 1)
        XCTAssertEqual(
            store.startedOver(accountIdB64: account),
            PeerPinStore.StartedOver(newKeyB64: new, keyGeneration: 2, signedAtMs: 5))
        // The old key closes its one-to-one; it is NOT a superseded key
        // (nothing is returned to the new account on its strength).
        XCTAssertEqual(store.loadStartedOverKeys(), [old: account])
        XCTAssertTrue(store.isStartedOverKey(old))
        XCTAssertFalse(store.isStartedOverKey(new))
        XCTAssertNil(store.loadSupersededKeys()[old])

        // Seen again: nothing to say twice.
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .already)
        // A later unsealed reset replaces the pending one; an older does not.
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: newer, keyGeneration: 3, signedAtMs: 9),
            .recorded)
        XCTAssertEqual(store.startedOver(accountIdB64: account)?.newKeyB64, newer)
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .ignored)
        XCTAssertEqual(store.loadStartedOverKeys(), [old: account], "the old key is listed once")
    }

    func testNothingIsRecordedForAnAccountThisDeviceDoesNotPinOrAnotherHeldKey() {
        let store = pins()
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: "b3RoZXI=", heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .ignored, "a group member known only by a verified binding")
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: newer, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .ignored, "the held key is not the pin")
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: old, keyGeneration: 2, signedAtMs: 5),
            .ignored)
        XCTAssertTrue(store.loadStartedOverKeys().isEmpty)
    }

    /// Accepting the contact request of the new keys is the person's own
    /// decision to trust them: the pin moves then, and only to the keys the
    /// pending reset installed.
    func testAcceptingTheRequestOfTheNewKeysAdoptsThemAndTheOldChatStaysClosed() {
        let store = pins()
        store.noteStartedOver(
            accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5)

        XCTAssertFalse(store.adoptStartedOver(accountIdB64: account, newKeyB64: newer, nowMs: 30_000), "another key")
        XCTAssertEqual(store.readPin(accountIdB64: account)?.pinnedSignaturePubkeyB64, old)

        XCTAssertTrue(store.adoptStartedOver(accountIdB64: account, newKeyB64: new, nowMs: 30_000))
        let pin = store.readPin(accountIdB64: account)
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, new)
        XCTAssertEqual(store.acceptedGeneration(accountIdB64: account), 2)
        XCTAssertNil(store.startedOver(accountIdB64: account), "nothing is pending any more")
        XCTAssertEqual(store.loadStartedOverKeys(), [old: account], "the old one-to-one stays closed")
        XCTAssertNil(store.loadSupersededKeys()[old], "and is still not the same person's retired key")
        XCTAssertFalse(store.adoptStartedOver(accountIdB64: account, newKeyB64: new, nowMs: 30_000), "once")
    }

    // MARK: - Alignment items D, E, F

    /// Item D: adding the person by name takes their new account as a new
    /// contact; the old one-to-one stays closed; a reset not pending for
    /// those keys refuses.
    func testAddingAPersonWhoStartedOverByNameTakesTheirNewAccount() {
        let store = pins()
        XCTAssertFalse(StartedOverAdd.adopt(
            ContactStartedOver(accountIdB64: account, newKeyB64: new), pins: store, nowMs: 1), "nothing pending")
        store.noteStartedOver(
            accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5)
        XCTAssertFalse(StartedOverAdd.adopt(
            ContactStartedOver(accountIdB64: account, newKeyB64: newer), pins: store, nowMs: 1), "other keys")
        XCTAssertTrue(StartedOverAdd.adopt(
            ContactStartedOver(accountIdB64: account, newKeyB64: new), pins: store, nowMs: 30_000))
        XCTAssertEqual(store.readPin(accountIdB64: account)?.pinnedSignaturePubkeyB64, new)
        XCTAssertEqual(store.loadStartedOverKeys(), [old: account], "the old one-to-one stays closed")
        XCTAssertTrue(SentContactRequest(
            recipientUsername: "x", recipientAccountIdB64: account, messageId: 1, createdAt: "",
            startedOver: true).startedOver == true)
    }

    /// Item E: what waits for the old one-to-one is never sent.
    func testWhatWaitsForTheOldOneToOneIsNotSent() {
        XCTAssertEqual(StartedOverChat.unsent(oneToOne: true, peerKeyB64: old, startedOverKeysB64: [old]), .notSent)
        XCTAssertEqual(StartedOverChat.unsent(oneToOne: true, peerKeyB64: new, startedOverKeysB64: [old]), .send)
        XCTAssertEqual(StartedOverChat.unsent(oneToOne: false, peerKeyB64: old, startedOverKeysB64: [old]), .send)
        XCTAssertEqual(StartedOverChat.unsent(oneToOne: true, peerKeyB64: nil, startedOverKeysB64: [old]), .send)
    }

    /// Item F: the pairing state bundle carries the floors and the closed
    /// keys as the agreed JSON, and the receiving device keeps the higher
    /// floor and adds the closed key it does not have.
    func testThePairingBundleCarriesTheFloorsAndTheStartedOverKeys() throws {
        let id = Data(repeating: 0x0A, count: 16)
        let idB64 = id.base64EncodedString()
        let idHex = id.map { String(format: "%02x", $0) }.joined()
        let old = Data(repeating: 0x11, count: 32).base64EncodedString()
        let new = Data(repeating: 0x22, count: 32).base64EncodedString()
        let sender = pins("sender")
        sender.setPin(accountIdB64: idB64, pinnedSignaturePubkeyB64: old, rotationMarker: 1)
        sender.raiseSealSeqFloor(accountIdB64: idB64, seq: 7)
        sender.noteStartedOver(
            accountIdB64: idB64, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 1234)

        var bundle = StateBundle.V4(contacts: [], pins: [], knownGroupIdsB64: [])
        let floors = StateBundle.carriedSealFloors(sender)
        bundle.sealFloors = floors.isEmpty ? nil : floors
        bundle.startedOver = StateBundle.carriedStartedOver(sender)
        XCTAssertEqual(bundle.sealFloors, [.init(accountIdHex: idHex, seq: 7)])
        XCTAssertEqual(bundle.startedOver, [.init(accountIdHex: idHex, oldKeyB64: old, atMs: 1234)])

        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: StateBundle.encode(bundle)) as? [String: Any])
        let floor = try XCTUnwrap((json["sealFloors"] as? [[String: Any]])?.first)
        XCTAssertEqual(floor["accountIdHex"] as? String, idHex)
        XCTAssertEqual(floor["seq"] as? Int, 7)
        let closed = try XCTUnwrap((json["startedOver"] as? [[String: Any]])?.first)
        XCTAssertEqual(closed["oldKeyB64"] as? String, old)
        XCTAssertEqual(closed["atMs"] as? Int, 1234)
        let decoded = try XCTUnwrap(StateBundle.decode(StateBundle.encode(bundle)))
        XCTAssertEqual(decoded.startedOver, bundle.startedOver)

        // Absent when empty.
        let empty = try XCTUnwrap(JSONSerialization.jsonObject(
            with: StateBundle.encode(StateBundle.V4(contacts: [], pins: [], knownGroupIdsB64: []))) as? [String: Any])
        XCTAssertNil(empty["sealFloors"])
        XCTAssertNil(empty["startedOver"])

        let receiver = pins("receiver")
        receiver.setPin(accountIdB64: idB64, pinnedSignaturePubkeyB64: old, rotationMarker: 1)
        receiver.raiseSealSeqFloor(accountIdB64: idB64, seq: 9)
        XCTAssertEqual(StateBundle.applySealFacts(decoded, pins: receiver), 1, "the closed key only")
        XCTAssertEqual(receiver.sealSeqFloor(accountIdB64: idB64), 9, "the higher seq is kept")
        XCTAssertEqual(receiver.loadStartedOverKeys(), [old: idB64])
        XCTAssertEqual(StateBundle.applySealFacts(decoded, pins: receiver), 0, "nothing twice")
        // A malformed entry refuses the whole bundle (the deviceCards rule,
        // as on Android).
        func refused(_ change: (inout StateBundle.V4) -> Void) -> Bool {
            var junk = decoded
            change(&junk)
            return StateBundle.decode(StateBundle.encode(junk)) == nil
        }
        XCTAssertTrue(refused { $0.startedOver = [.init(accountIdHex: "zz", oldKeyB64: old, atMs: 1)] })
        XCTAssertTrue(refused { $0.startedOver = [.init(accountIdHex: idHex, oldKeyB64: "AAEC", atMs: 1)] })
        XCTAssertTrue(refused { $0.startedOver = [.init(accountIdHex: idHex, oldKeyB64: old, atMs: -1)] })
        XCTAssertTrue(refused { $0.sealFloors = [.init(accountIdHex: idHex.uppercased(), seq: 1)] })
        XCTAssertTrue(refused { $0.sealFloors = [.init(accountIdHex: idHex, seq: 1), .init(accountIdHex: idHex, seq: 2)] })
        XCTAssertTrue(refused {
            $0.sealFloors = (0...StateBundle.maxSealFloors).map {
                .init(accountIdHex: String(format: "%032lx", $0), seq: 1)
            }
        })
        XCTAssertFalse(refused { _ in }, "the well-formed bundle decodes")
    }

    /// UX 16.11 follow-up: the old conversation's person (a started-over
    /// key) is offered no `Add to contacts`, also after the contact was
    /// removed (the removed-person card offers it otherwise).
    func testTheCardOfAStartedOverKeyOffersNoAddToContacts() {
        let removed = personInfoSections(caps: PersonInfoCapabilities(
            membershipEnded: false, isContact: false, contactsOfContactsOn: false, isBlocked: false))
        XCTAssertTrue(removed.contains(.addContact), "a removed person is offered it")
        XCTAssertFalse(StartedOverCard.sections(removed, keyStartedOver: true).contains(.addContact))
        XCTAssertEqual(StartedOverCard.sections(removed, keyStartedOver: false), removed)
        XCTAssertEqual(
            StartedOverCard.sections(removed, keyStartedOver: true),
            removed.filter { $0 != .addContact }, "nothing else changes")
    }

    // MARK: - The two surfaces (UX 16.11)

    func testOnlyARequestFromTheNewKeysCarriesTheNoteAndAdoptsThem() {
        XCTAssertTrue(StartedOverRequest.isFromNewKeys(requestKeyB64: new, newKeyB64: new))
        XCTAssertFalse(StartedOverRequest.isFromNewKeys(requestKeyB64: old, newKeyB64: new))
        XCTAssertFalse(StartedOverRequest.isFromNewKeys(requestKeyB64: "", newKeyB64: ""))
        XCTAssertTrue(StartedOverRequest.adoptsNewKeys(requestKeyB64: new, pendingNewKeyB64: new))
        XCTAssertFalse(StartedOverRequest.adoptsNewKeys(requestKeyB64: new, pendingNewKeyB64: nil))
        XCTAssertFalse(StartedOverRequest.adoptsNewKeys(requestKeyB64: old, pendingNewKeyB64: new))
        XCTAssertNotEqual(Localization.text(StartedOverRequest.noteKey), StartedOverRequest.noteKey)
    }

    func testTheOldOneToOneLosesItsComposerAndItsCallButtons() {
        XCTAssertEqual(
            StartedOverChat.bottom(oneToOne: true, peerKeyB64: old, startedOverKeysB64: [old]), .closedLine)
        XCTAssertEqual(
            StartedOverChat.bottom(oneToOne: true, peerKeyB64: new, startedOverKeysB64: [old]), .composer,
            "the new one-to-one is an ordinary chat")
        XCTAssertEqual(
            StartedOverChat.bottom(oneToOne: false, peerKeyB64: old, startedOverKeysB64: [old]), .composer,
            "a group is never closed for a member who started over")
        XCTAssertEqual(
            StartedOverChat.bottom(oneToOne: true, peerKeyB64: nil, startedOverKeysB64: [old]), .composer)

        // The line names the person; the composer and its draft are gone.
        let notice = Localization.text(StartedOverChat.closedKey, texts: ["name": "Anya"])
        XCTAssertTrue(notice.contains("Anya"))
        XCTAssertEqual(
            ChatViewModel.composerSlot(peerDeleted: false, startedOverNotice: notice, membership: .active, draft: "hi"),
            .withdrawn(notice: notice, draft: nil, draftLabel: nil))
        XCTAssertEqual(
            ChatViewModel.composerSlot(peerDeleted: false, startedOverNotice: nil, membership: .active, draft: "hi"),
            .composer)
        // A deleted account's line wins (there is nobody at all).
        XCTAssertEqual(
            ChatViewModel.composerSlot(peerDeleted: true, startedOverNotice: notice, membership: .active, draft: ""),
            .withdrawn(notice: ResolvedDisplayName.deletedBody, draft: nil, draftLabel: nil))
        XCTAssertFalse(ChatViewModel.hasSomeoneToCall(
            peerDeleted: false, peerStartedOver: true, isGroup: false, people: 2))
        XCTAssertTrue(ChatViewModel.hasSomeoneToCall(
            peerDeleted: false, peerStartedOver: false, isGroup: false, people: 2))
    }

    // MARK: - Saving a phrase changes no key (UX 19.5)

    func testSavingAPhraseShowsTheWordsTheDeviceHoldsAndCountsOnlyOncePublished() async {
        var published = 0
        var publishes = [false, true]
        let flow = SavePhraseFlow(ports: .init(
            heldPhrase: { "one two three" },
            publishPhraseSeal: {
                published += 1
                return publishes.removeFirst()
            }))
        XCTAssertEqual(flow.state, .idle)
        // Not before the words are on screen.
        let early = await flow.confirmHeldWords()
        XCTAssertFalse(early)
        XCTAssertEqual(published, 0)

        flow.primary()
        XCTAssertEqual(flow.state, .heldWords("one two three"), "the words appear at once: no key changes")

        // The three-word check passed and the publish did not land: the
        // screen stays, with Try again; nothing was saved.
        let first = await flow.confirmHeldWords()
        XCTAssertFalse(first)
        XCTAssertTrue(flow.confirmFailed)
        XCTAssertEqual(flow.state, .heldWords("one two three"))
        let second = await flow.confirmHeldWords()
        XCTAssertTrue(second)
        XCTAssertFalse(flow.confirmFailed)
        XCTAssertEqual(published, 2)
    }

    func testADeviceThatDoesNotHoldTheAccountsWordsSavesNothing() {
        let flow = SavePhraseFlow(ports: .init(heldPhrase: { nil }, publishPhraseSeal: { true }))
        flow.primary()
        XCTAssertEqual(flow.state, .failed)
    }
}
