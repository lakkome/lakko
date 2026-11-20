// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0074 section 3.7 (2026-11-20) — an owner appoints a member that rotated
/// its account keys, and that member opens what the owner sealed. Driven
/// through the real core: the link is signed and verified by
/// `signConversationLink` / `verifyConversationLink`, and the envelopes sealed
/// and opened by the registrar-seed and knock-seed functions.
final class RotatedMemberSealTests: XCTestCase {
    private struct Person {
        let conversation: Identity
        let current: Identity
        let other: Identity
        let link: Data
    }

    private func person() throws -> Person {
        let conversation = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let current = try Identity.fromSeed(seed: Data(repeating: 0x32, count: 32))
        let other = try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32))
        let link = try signConversationLink(
            conversationEd25519Seed: conversation.ed25519Seed(),
            currentEd25519Seed: current.ed25519Seed(),
            issuedAtUnixSecs: 1_700_000_000)
        return Person(conversation: conversation, current: current, other: other, link: link)
    }

    private func serves(_ served: Data, _ link: Data?, _ roster: Data) -> Bool {
        RotatedMemberSeal.bundleServesRosterKey(
            servedKey: served, conversationLink: link, rosterKey: roster,
            verifyLink: { try verifyConversationLink(link: $0, trustedCurrentKey: $1) })
    }

    func testAnUnrotatedMemberIsAcceptedOnTheServedKeyAlone() throws {
        let p = try person()
        let key = p.conversation.signaturePublicKey()
        XCTAssertTrue(serves(key, nil, key))
    }

    func testARotatedMemberWhoseLinkNamesTheRosterKeyIsAccepted() throws {
        let p = try person()
        XCTAssertTrue(serves(p.current.signaturePublicKey(), p.link, p.conversation.signaturePublicKey()))
    }

    func testARotatedMemberWithNoLinkIsRefused() throws {
        let p = try person()
        XCTAssertFalse(serves(p.current.signaturePublicKey(), nil, p.conversation.signaturePublicKey()))
        XCTAssertFalse(serves(p.current.signaturePublicKey(), Data(), p.conversation.signaturePublicKey()))
    }

    func testALinkThatDoesNotVerifyAgainstTheServedKeyIsRefused() throws {
        let p = try person()
        XCTAssertFalse(serves(p.other.signaturePublicKey(), p.link, p.conversation.signaturePublicKey()))
        var damaged = p.link
        damaged[damaged.count - 1] ^= 1
        XCTAssertFalse(serves(p.current.signaturePublicKey(), damaged, p.conversation.signaturePublicKey()))
    }

    func testALinkNamingAnotherConversationKeyIsRefused() throws {
        let p = try person()
        XCTAssertFalse(serves(p.current.signaturePublicKey(), p.link, p.other.signaturePublicKey()))
    }

    func testTheReceiverOpensARegistrarSeedSealedToItsCurrentKey() throws {
        let p = try person()
        let roster = p.conversation.signaturePublicKey()
        let seed = try dsJournalRegistrarSeed(rootSeed: Data(repeating: 0x51, count: 32), memberAccountKey: roster)
        let envelope = try composeRegistrarSeedEnvelope(
            journalId: Data(repeating: 0x45, count: 32), memberAccountKey: roster,
            memberX25519PublicKey: p.current.x25519PublicKey(), rank: .member, registrarSeed: seed)
        func open(current: Identity?) -> Data? {
            RotatedMemberSeal.openSealedToOwnAccount(
                conversation: p.conversation, current: current,
                openOwn: { try openRegistrarSeedEnvelope(plaintext: envelope, accountIdentity: $0) },
                openSealedTo: {
                    try openRegistrarSeedEnvelopeSealedTo(
                        plaintext: envelope, accountIdentity: $0, sealedToIdentity: $1)
                })
        }
        XCTAssertEqual(open(current: p.current), seed)
        // An unrotated receiver tries only its own key, which does not open it.
        XCTAssertNil(open(current: nil))
    }

    func testAnEnvelopeSealedToTheConversationKeyStillOpens() throws {
        let p = try person()
        let roster = p.conversation.signaturePublicKey()
        let seed = try dsJournalRegistrarSeed(rootSeed: Data(repeating: 0x52, count: 32), memberAccountKey: roster)
        let envelope = try composeRegistrarSeedEnvelope(
            journalId: Data(repeating: 0x46, count: 32), memberAccountKey: roster,
            memberX25519PublicKey: p.conversation.x25519PublicKey(), rank: .admin, registrarSeed: seed)
        let opened = RotatedMemberSeal.openSealedToOwnAccount(
            conversation: p.conversation, current: p.current,
            openOwn: { try openRegistrarSeedEnvelope(plaintext: envelope, accountIdentity: $0) },
            openSealedTo: {
                try openRegistrarSeedEnvelopeSealedTo(plaintext: envelope, accountIdentity: $0, sealedToIdentity: $1)
            })
        XCTAssertEqual(opened, seed)
    }

    func testTheReceiverOpensAKnockSeedSealedToItsCurrentKeyAndTheAddressIsChecked() throws {
        let p = try person()
        let seed = try groupKnockSeed(journalRootSeed: Data(repeating: 0x11, count: 32), version: 1)
        let envelope = try composeKnockSeedEnvelope(
            journalId: Data(repeating: 0x4A, count: 32), memberAccountKey: p.conversation.signaturePublicKey(),
            memberX25519PublicKey: p.current.x25519PublicKey(), knockVersion: 1, knockSeed: seed)
        func open(conversation: Identity) -> Data? {
            RotatedMemberSeal.openSealedToOwnAccount(
                conversation: conversation, current: p.current,
                openOwn: { try openKnockSeedEnvelope(plaintext: envelope, accountIdentity: $0) },
                openSealedTo: {
                    try openKnockSeedEnvelopeSealedTo(plaintext: envelope, accountIdentity: $0, sealedToIdentity: $1)
                })
        }
        XCTAssertEqual(open(conversation: p.conversation), seed)
        XCTAssertNil(open(conversation: p.other))
    }
}
