// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073 R.2 with rule 4 (ADR-0101 section 10) — the acceptance rule for a
/// contact's attested key reset, the verified binding of a member this device
/// never pinned, the re-pin and the contact row after it. The table itself is
/// the core's (`lakko_contacts::reset_verdict`, reached through the FFI); the
/// cases below are its rows asked through the Swift shapes `ContactsClient`
/// gathers, so a mapping that hands the rule the wrong input fails here.
final class ResetVerdictTests: XCTestCase {

    private let old = "b2xk"
    private let new = "bmV3"

    private let sealedBy = SealCheck(valid: true, announcementSeq: 7)

    private func input(
        accepted: UInt64 = 1,
        bundle: UInt64 = 2,
        attest: AttestCheck = .verified(keyGeneration: 2, signedAtMs: 1_000),
        log: LogCheck = LogCheck(status: .off, isReset: nil),
        fetched: String? = nil,
        seal: SealCheck? = nil
    ) -> ResetVerdictInput {
        ResetVerdictInput(
            heldKeyB64: old, acceptedGeneration: accepted, fetchedKeyB64: fetched ?? new,
            bundleGeneration: bundle, attest: attest, log: log, seal: seal ?? sealedBy)
    }

    // MARK: - resetVerdict

    func testRules1To4WithTheLogOffAreTheSamePerson() {
        XCTAssertEqual(
            resetVerdict(input()), .sealed(keyGeneration: 2, signedAtMs: 1_000, announcementSeq: 7))
    }

    /// ADR-0101 section 10 — rules 1 to 3 without a seal: a new account
    /// under the same username. Never the same person, never a refusal.
    func testAResetWithoutASealIsUnsealed() {
        XCTAssertEqual(
            resetVerdict(input(seal: SealCheck.none)), .unsealed(keyGeneration: 2, signedAtMs: 1_000))
        XCTAssertEqual(
            resetVerdict(input(seal: SealCheck(valid: false, refusal: "seqBelowFloor"))),
            .unsealed(keyGeneration: 2, signedAtMs: 1_000))
        // The default of the Swift shape is "no seal": an input built
        // without one can never read as sealed.
        let bare = ResetVerdictInput(
            heldKeyB64: old, acceptedGeneration: 1, fetchedKeyB64: new, bundleGeneration: 2,
            attest: .verified(keyGeneration: 2, signedAtMs: 1_000), log: LogCheck(status: .off, isReset: nil))
        XCTAssertEqual(resetVerdict(bare), .unsealed(keyGeneration: 2, signedAtMs: 1_000))
    }

    /// With a key log configured the seal counts only when the log holds
    /// the announcement; an unreachable log decides nothing.
    func testWithALogTheSealNeedsItsAnnouncementInTheLog() {
        let confirmed = LogCheck(status: .confirmed, isReset: true)
        XCTAssertEqual(
            resetVerdict(input(log: confirmed, seal: SealCheck(valid: true, announcementSeq: 7, log: .proven))),
            .sealed(keyGeneration: 2, signedAtMs: 1_000, announcementSeq: 7))
        XCTAssertEqual(
            resetVerdict(input(log: confirmed, seal: SealCheck(valid: true, announcementSeq: 7, log: .notProven))),
            .unsealed(keyGeneration: 2, signedAtMs: 1_000))
        XCTAssertEqual(
            resetVerdict(input(log: confirmed, seal: SealCheck(valid: true, announcementSeq: 7, log: .unreachable))),
            .noVerdict)
        // With the log off its half is ignored.
        XCTAssertEqual(
            resetVerdict(input(seal: SealCheck(valid: true, announcementSeq: 7, log: .notProven))),
            .sealed(keyGeneration: 2, signedAtMs: 1_000, announcementSeq: 7))
    }

    /// A seal never rescues a reset the server's word does not carry.
    func testASealDoesNotStandInForRules1To3() {
        XCTAssertEqual(resetVerdict(input(attest: .absent)), .refused(.unconfirmed))
        XCTAssertEqual(resetVerdict(input(attest: .invalid)), .refused(.unconfirmed))
        XCTAssertEqual(resetVerdict(input(accepted: 2)), .refused(.unconfirmed))
        XCTAssertEqual(
            resetVerdict(input(log: LogCheck(status: .mismatch, isReset: true))), .refused(.unconfirmed))
    }

    func testRow1NoAttestationIsRefusedWhateverTheLogSays() {
        for status in [LogVerifyStatus.off, .confirmed, .absent, .unreachable, .mismatch, .rollback] {
            XCTAssertEqual(
                resetVerdict(input(attest: .absent, log: LogCheck(status: status, isReset: true))),
                .refused(.unconfirmed), "\(status)")
        }
    }

    func testRow1NoAttestationAndALogThatShowsNoResetIsUnexplained() {
        XCTAssertEqual(
            resetVerdict(input(attest: .absent, log: LogCheck(status: .confirmed, isReset: false))),
            .refused(.unexplained))
    }

    func testRow2AnAttestationThatFailsABindingIsRefused() {
        XCTAssertEqual(resetVerdict(input(attest: .invalid)), .refused(.unconfirmed))
    }

    func testRow3NoAttestKeyIsNoVerdictNotARefusal() {
        XCTAssertEqual(resetVerdict(input(attest: .noKey)), .noVerdict)
    }

    func testRow4TheAttestedGenerationMustEqualTheBundles() {
        XCTAssertEqual(resetVerdict(input(bundle: 3)), .refused(.unconfirmed))
    }

    func testRow4AGenerationNotAboveTheAcceptedOneIsRefused() {
        XCTAssertEqual(resetVerdict(input(accepted: 2)), .refused(.unconfirmed))
        XCTAssertEqual(resetVerdict(input(accepted: 3)), .refused(.unconfirmed))
    }

    func testRow5TheLogConfirmsTheKeyWithoutAResetIsUnexplained() {
        XCTAssertEqual(
            resetVerdict(input(log: LogCheck(status: .confirmed, isReset: false))),
            .refused(.unexplained))
        XCTAssertEqual(
            resetVerdict(input(log: LogCheck(status: .confirmed, isReset: nil))),
            .refused(.unconfirmed))
    }

    func testRow6ALogMismatchOrRollbackIsRefused() {
        XCTAssertEqual(
            resetVerdict(input(log: LogCheck(status: .mismatch, isReset: true))), .refused(.unconfirmed))
        XCTAssertEqual(
            resetVerdict(input(log: LogCheck(status: .rollback, isReset: nil))), .refused(.unconfirmed))
    }

    func testRow7AnUnreachableLogOrAnAbsentEntryIsNoVerdict() {
        XCTAssertEqual(resetVerdict(input(log: LogCheck(status: .unreachable, isReset: nil))), .noVerdict)
        XCTAssertEqual(resetVerdict(input(log: LogCheck(status: .absent, isReset: nil))), .noVerdict)
    }

    func testAnUnchangedKeyIsNothingToJudge() {
        XCTAssertEqual(resetVerdict(input(fetched: old)), .noVerdict)
    }

    func testAKeyThatIsNotBase64IsNothingToJudge() {
        XCTAssertEqual(resetVerdict(input(fetched: "not base64 !")), .noVerdict)
    }

    func testWithNoRecordedGenerationOnlyAGenerationAboveZeroIsAccepted() {
        XCTAssertEqual(
            resetVerdict(input(
                accepted: 0, bundle: 1, attest: .verified(keyGeneration: 1, signedAtMs: 5))),
            .sealed(keyGeneration: 1, signedAtMs: 5, announcementSeq: 7))
        XCTAssertEqual(
            resetVerdict(input(
                accepted: 0, bundle: 0, attest: .verified(keyGeneration: 0, signedAtMs: 5))),
            .refused(.unconfirmed))
    }

    func testTheRepinMarkerNeverMovesBackwards() {
        XCTAssertEqual(resetRepinMarker(heldMarker: 0, nowMs: 5_000), 5)
        XCTAssertEqual(resetRepinMarker(heldMarker: 9_000_000_000, nowMs: 5_000), 9_000_000_000)
    }

    // MARK: - memberHeldKey

    private let binding = VerifiedBinding(keyB64: "cm9zdGVy", keyGeneration: 4, verifiedAtMs: 1)

    func testAPinnedAccountIsDecidedByItsPin() {
        XCTAssertEqual(
            memberHeldKey(pinned: true, binding: binding, rosterKeyB64: "cm9zdGVy", fetchedKeyB64: "x"),
            .pin)
    }

    func testNoBindingAndTheDirectoryServesTheRosterKeyRecords() {
        XCTAssertEqual(
            memberHeldKey(pinned: false, binding: nil, rosterKeyB64: "cm9zdGVy", fetchedKeyB64: "cm9zdGVy"),
            .record)
    }

    func testTheAttackNoBindingAndAnotherKeyServedIsNoVerdict() {
        XCTAssertEqual(
            memberHeldKey(pinned: false, binding: nil, rosterKeyB64: "cm9zdGVy", fetchedKeyB64: "b3RoZXI="),
            MemberHeldVerdict.none)
    }

    func testABindingNamingTheRosterKeyStillServedIsConfirmed() {
        XCTAssertEqual(
            memberHeldKey(pinned: false, binding: binding, rosterKeyB64: "cm9zdGVy", fetchedKeyB64: "cm9zdGVy"),
            .confirmed)
    }

    func testABindingNamingTheRosterKeyWithAnotherKeyServedIsHeld() {
        XCTAssertEqual(
            memberHeldKey(pinned: false, binding: binding, rosterKeyB64: "cm9zdGVy", fetchedKeyB64: "b3RoZXI="),
            .held(heldKeyB64: "cm9zdGVy", acceptedGeneration: 4))
    }

    func testTheAttackLaterABindingNamingAnotherKeyNeverLendsItselfToTheRosterKey() {
        for fetched in ["cm9zdGVy", "bWVtYmVy", "b3RoZXI="] {
            XCTAssertEqual(
                memberHeldKey(pinned: false, binding: binding, rosterKeyB64: "bWVtYmVy", fetchedKeyB64: fetched),
                MemberHeldVerdict.none)
        }
    }

    // MARK: - VerifiedBindingStore

    private func freshDefaults() -> UserDefaults {
        let name = "reset-verdict-tests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    func testTheBindingStoreIsWriteOncePerAccountId() {
        let store = VerifiedBindingStore(defaults: freshDefaults(), ownerAccountIdB64: "me")
        XCTAssertTrue(store.record(accountIdB64: "a", binding: binding))
        XCTAssertFalse(store.record(
            accountIdB64: "a", binding: VerifiedBinding(keyB64: "b3RoZXI=", keyGeneration: 9, verifiedAtMs: 2)))
        XCTAssertEqual(store.read(accountIdB64: "a"), binding)
    }

    func testTheBindingStoreIsPerSignedInAccount() {
        let defaults = freshDefaults()
        VerifiedBindingStore(defaults: defaults, ownerAccountIdB64: "me").record(accountIdB64: "a", binding: binding)
        XCTAssertNil(VerifiedBindingStore(defaults: defaults, ownerAccountIdB64: "other").read(accountIdB64: "a"))
    }

    func testTheBindingStoreDropsOneBindingAndTheWholeRowWithTheAccount() {
        let defaults = freshDefaults()
        let store = VerifiedBindingStore(defaults: defaults, ownerAccountIdB64: "me")
        store.record(accountIdB64: "a", binding: binding)
        store.record(accountIdB64: "b", binding: binding)
        store.drop(accountIdB64: "a")
        XCTAssertNil(store.read(accountIdB64: "a"))
        XCTAssertNotNil(store.read(accountIdB64: "b"))
        VerifiedBindingStore.clear(ownerAccountIdB64: "me", defaults: defaults)
        XCTAssertNil(store.read(accountIdB64: "b"))
    }

    func testAJunkRowReadsAsNoBinding() {
        let defaults = freshDefaults()
        defaults.set(Data("not json".utf8), forKey: VerifiedBindingStore.prefix + "me")
        XCTAssertNil(VerifiedBindingStore(defaults: defaults, ownerAccountIdB64: "me").read(accountIdB64: "a"))
    }

    // MARK: - PeerPinStore.acceptReset

    private func reset(_ generation: UInt64, new: String = "bmV3", held: String = "b2xk") -> PeerPinStore.AcceptedReset {
        PeerPinStore.AcceptedReset(
            accountIdB64: "acct", heldKeyB64: held, newKeyB64: new,
            keyGeneration: generation, signedAtMs: 77, nowMs: 5_000)
    }

    func testAcceptResetRepinsAndRetiresTheHeldKey() {
        let pins = PeerPinStore(defaults: freshDefaults(), ownerAccountIdB64: "me")
        pins.setPin(accountIdB64: "acct", pinnedSignaturePubkeyB64: old, rotationMarker: 0, keyGeneration: 1)
        pins.recordLogStatus(accountIdB64: "acct", logStatus: "confirmed")
        XCTAssertEqual(pins.acceptedGeneration(accountIdB64: "acct"), 1)

        XCTAssertEqual(pins.acceptReset(reset(2), retiredPredecessorsB64: ["cHJlZA=="]), .accepted)
        let pin = pins.readPin(accountIdB64: "acct")
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, new)
        XCTAssertEqual(pin?.acceptedKeyGeneration, 2)
        XCTAssertEqual(pin?.rotationMarker, 5)
        XCTAssertEqual(pin?.previousKeysB64, ["cHJlZA==", old])
        XCTAssertEqual(pin?.supersededKeysB64, ["cHJlZA==", old])
        XCTAssertEqual(pin?.resetSignedAtMs, 77)
        XCTAssertNil(pin?.logStatus, "the log verdict described the old key")
        XCTAssertEqual(pins.loadSupersededKeys(), ["cHJlZA==": "acct", old: "acct"])
    }

    func testAcceptResetRefusesAReplayAndARollback() {
        let pins = PeerPinStore(defaults: freshDefaults(), ownerAccountIdB64: "me")
        pins.setPin(accountIdB64: "acct", pinnedSignaturePubkeyB64: old, rotationMarker: 0, keyGeneration: 2)
        XCTAssertEqual(pins.acceptReset(reset(2)), .stale)
        XCTAssertEqual(pins.acceptReset(reset(1)), .stale)
        XCTAssertEqual(pins.readPin(accountIdB64: "acct")?.pinnedSignaturePubkeyB64, old)
        XCTAssertEqual(pins.acceptReset(reset(3)), .accepted)
        XCTAssertEqual(pins.acceptReset(reset(3)), .already)
        // A second reset accumulates the aliases.
        XCTAssertEqual(pins.acceptReset(reset(4, new: "bmV3Mg==", held: new)), .accepted)
        XCTAssertEqual(pins.readPin(accountIdB64: "acct")?.supersededKeysB64, [old, new])
    }

    func testAcceptResetFromABindingWritesAPinForAnAccountNeverPinned() {
        let pins = PeerPinStore(defaults: freshDefaults(), ownerAccountIdB64: "me")
        XCTAssertEqual(pins.acceptReset(reset(2)), .accepted)
        XCTAssertEqual(pins.readPin(accountIdB64: "acct")?.supersededKeysB64, [old])
    }

    func testSetPinKeepsTheAcceptedGenerationAndTheAliases() {
        let pins = PeerPinStore(defaults: freshDefaults(), ownerAccountIdB64: "me")
        pins.setPin(accountIdB64: "acct", pinnedSignaturePubkeyB64: old, rotationMarker: 0, keyGeneration: 1)
        XCTAssertEqual(pins.acceptReset(reset(2)), .accepted)
        pins.recordLogVersion(accountIdB64: "acct", version: 9)
        pins.recordLogReset(accountIdB64: "acct", isReset: true)
        pins.setPin(accountIdB64: "acct", pinnedSignaturePubkeyB64: "cm90", rotationMarker: 10)
        let pin = pins.readPin(accountIdB64: "acct")
        XCTAssertEqual(pin?.acceptedKeyGeneration, 2)
        XCTAssertEqual(pin?.supersededKeysB64, [old])
        XCTAssertEqual(pin?.pinnedLogVersion, 9)
    }

    // MARK: - ContactRequestGate

    func testARequestToKeysInHandIsCheckedOnlyForAPinnedAccountUnderAnotherKey() {
        XCTAssertEqual(ContactRequestGate.decide(pinnedKeyB64: nil, recipientKeyB64: "k"), .send)
        XCTAssertEqual(ContactRequestGate.decide(pinnedKeyB64: "k", recipientKeyB64: "k"), .send)
        XCTAssertEqual(ContactRequestGate.decide(pinnedKeyB64: "p", recipientKeyB64: "k"), .checkIdentity)
    }

    // MARK: - ContactRekey

    private func contact(_ id: Int64, key: UInt8, account: UInt8?, name: String? = nil) -> AcceptedContact {
        AcceptedContact(
            request: IncomingContactRequest(
                messageId: id,
                fromPubkey: Data(repeating: key, count: 32),
                fromAccountId: account.map { Data(repeating: $0, count: 16) },
                fromPseudonym: Data(repeating: 0, count: 32),
                validAfterMs: 0, intro: nil, profile: nil, createdAt: ""),
            groupIdHex: "ab", displayName: name)
    }

    func testTheContactRowIsRekeyedKeepingTheRenameAndTheChatLink() {
        let accountB64 = base64Encode(Data(repeating: 0xA1, count: 16))
        let oldKey = Data(repeating: 1, count: 32)
        let newKey = Data(repeating: 2, count: 32)
        let out = ContactRekey.afterReset(
            accepted: [contact(7, key: 1, account: 0xA1, name: "Anna"), contact(8, key: 9, account: 0xB2)],
            accountIdB64: accountB64, previousKeyB64: base64Encode(oldKey), newKey: newKey)
        XCTAssertEqual(out.count, 2)
        XCTAssertEqual(out[0].request.fromPubkey, newKey)
        XCTAssertEqual(out[0].request.messageId, 7)
        XCTAssertEqual(out[0].displayName, "Anna")
        XCTAssertEqual(out[0].groupIdHex, "ab")
        XCTAssertEqual(out[0].previousKeysB64, [base64Encode(oldKey)])
        XCTAssertEqual(out[1], contact(8, key: 9, account: 0xB2))
    }

    func testASecondRowUnderTheNewKeyFoldsIntoThePersonsRow() {
        let accountB64 = base64Encode(Data(repeating: 0xA1, count: 16))
        let out = ContactRekey.afterReset(
            accepted: [contact(7, key: 1, account: 0xA1), contact(9, key: 2, account: nil)],
            accountIdB64: accountB64, previousKeyB64: base64Encode(Data(repeating: 1, count: 32)),
            newKey: Data(repeating: 2, count: 32))
        XCTAssertEqual(out.map(\.request.messageId), [7])
    }

    func testARowFoundByThePreviousKeyAloneIsRekeyedAndAnUnknownAccountChangesNothing() {
        let rows = [contact(7, key: 1, account: nil)]
        let out = ContactRekey.afterReset(
            accepted: rows, accountIdB64: "unknown",
            previousKeyB64: base64Encode(Data(repeating: 1, count: 32)), newKey: Data(repeating: 2, count: 32))
        XCTAssertEqual(out[0].request.fromPubkey, Data(repeating: 2, count: 32))
        XCTAssertEqual(
            ContactRekey.afterReset(
                accepted: rows, accountIdB64: "unknown", previousKeyB64: "eA==",
                newKey: Data(repeating: 2, count: 32)),
            rows)
    }
}
