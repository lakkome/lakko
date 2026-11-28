// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0101 section 10 — the account's seal announcement and the reset seal
/// on this device: which entries the next announcement lists, what the
/// device reads back from it, whether a reset made now would be sealed
/// (UX 15.1c), and the publish itself over scripted ports. The codecs and
/// the checks are the core's and are used here unmocked, so an announcement
/// this device publishes is the one a contact's device verifies.
@MainActor
final class ResetSealsTests: XCTestCase {

    private let accountId = Data(repeating: 0xA1, count: 16)
    private let accountIdB64 = Data(repeating: 0xA1, count: 16).base64EncodedString()
    private let exportKey = Data(repeating: 0x11, count: 64)
    private let otherExportKey = Data(repeating: 0x22, count: 64)
    private let credentialId = Data([1, 2, 3, 4])
    private let prf = Data(repeating: 0x33, count: 32)

    private func defaults(_ name: String = #function) -> UserDefaults {
        let suite = "lakko.tests.reset-seals.\(name)"
        UserDefaults().removePersistentDomain(forName: suite)
        return UserDefaults(suiteName: suite)!
    }

    private func passwordEntry(_ key: Data? = nil) throws -> FfiSealEntry {
        try ResetSealKey.fromPasswordExportKey(exportKey: key ?? exportKey).announcementEntry(credentialId: nil)
    }

    private func passkeyEntry(_ id: Data? = nil, prf secret: Data? = nil) throws -> FfiSealEntry {
        try ResetSealKey.fromPasskeyPrf(prfOutput: secret ?? prf).announcementEntry(credentialId: id ?? credentialId)
    }

    private func phraseEntry(_ mnemonic: RecoveryMnemonic) throws -> FfiSealEntry {
        try ResetSealKey.fromMnemonic(mnemonic: mnemonic).announcementEntry(credentialId: nil)
    }

    private func announcement(
        _ entries: [FfiSealEntry], seq: UInt64 = 1, generation: UInt64 = 1
    ) -> FfiSealAnnouncement {
        FfiSealAnnouncement(
            accountRef: Data(repeating: 0, count: 32), keyGeneration: generation, seq: seq, issuedAtMs: 1,
            entries: entries)
    }

    // MARK: - The plan

    func testAFirstAnnouncementListsThePasswordAlone() throws {
        let outcome = SealAnnouncementPlan.decide(
            current: nil, highestSeq: 0, passwordEntry: try passwordEntry(), phraseEntry: nil, change: .landing)
        XCTAssertEqual(outcome, .publish(entries: [try passwordEntry()], seq: 1))
    }

    /// The phrase the device holds is listed only once it was SAVED: a
    /// landing never lists it by itself.
    func testALandingNeverListsAnUnsavedPhrase() throws {
        let words = try phraseEntry(RecoveryMnemonic.generate())
        let outcome = SealAnnouncementPlan.decide(
            current: nil, highestSeq: 0, passwordEntry: try passwordEntry(), phraseEntry: words, change: .landing)
        XCTAssertEqual(outcome, .publish(entries: [try passwordEntry()], seq: 1))
        // ...nor does a landing touch an announcement that stands.
        let stands = announcement([try passwordEntry(otherExportKey)], seq: 4)
        XCTAssertEqual(
            SealAnnouncementPlan.decide(
                current: stands, highestSeq: 4, passwordEntry: try passwordEntry(), phraseEntry: words,
                change: .landing),
            .nothing, "a device with an older export key does not replace the announced password seal")
    }

    func testWithoutAnExportKeyAndWithNoAnnouncedPasswordSealNothingIsPublished() {
        XCTAssertEqual(
            SealAnnouncementPlan.decide(
                current: nil, highestSeq: 0, passwordEntry: nil, phraseEntry: nil, change: .landing),
            .noPasswordSeal)
        XCTAssertEqual(
            SealAnnouncementPlan.decide(
                current: nil, highestSeq: 0, passwordEntry: nil, phraseEntry: nil,
                change: .passkeyAdded(credentialId: credentialId, prfOutput: prf)),
            .noPasswordSeal, "an announcement lists exactly one password entry")
    }

    func testAPasswordProvenHereReplacesTheAnnouncedOneAndSeqGoesAboveEveryOneSeen() throws {
        let stands = announcement([try passwordEntry(otherExportKey), try passkeyEntry()], seq: 4)
        let outcome = SealAnnouncementPlan.decide(
            current: stands, highestSeq: 9, passwordEntry: try passwordEntry(), phraseEntry: nil,
            change: .password)
        guard case .publish(let entries, let seq) = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(seq, 10)
        XCTAssertEqual(Set(entries), [try passwordEntry(), try passkeyEntry()], "the passkey seal is kept")
        XCTAssertEqual(entries.filter { $0.kind == .password }.count, 1)
        // The same password again: nothing to say.
        XCTAssertEqual(
            SealAnnouncementPlan.decide(
                current: announcement(entries, seq: 10), highestSeq: 10, passwordEntry: try passwordEntry(),
                phraseEntry: nil, change: .password),
            .nothing)
    }

    func testAPasskeyIsListedWithItsTagReplacedOnReAddAndTakenOutWhenRemoved() throws {
        let stands = announcement([try passwordEntry()], seq: 1)
        let added = SealAnnouncementPlan.decide(
            current: stands, highestSeq: 1, passwordEntry: try passwordEntry(), phraseEntry: nil,
            change: .passkeyAdded(credentialId: credentialId, prfOutput: prf))
        guard case .publish(let entries, 2) = added else { return XCTFail("\(added)") }
        XCTAssertEqual(Set(entries), [try passwordEntry(), try passkeyEntry()])
        XCTAssertEqual(
            entries.first { $0.kind == .passkey }?.tag, resetSealPasskeyTag(credentialId: credentialId))

        // The same credential with another secret replaces its entry.
        let other = Data(repeating: 0x44, count: 32)
        let again = SealAnnouncementPlan.decide(
            current: announcement(entries, seq: 2), highestSeq: 2, passwordEntry: nil, phraseEntry: nil,
            change: .passkeyAdded(credentialId: credentialId, prfOutput: other))
        guard case .publish(let replaced, 3) = again else { return XCTFail("\(again)") }
        XCTAssertEqual(replaced.filter { $0.kind == .passkey }, [try passkeyEntry(prf: other)])

        let removed = SealAnnouncementPlan.decide(
            current: announcement(entries, seq: 2), highestSeq: 2, passwordEntry: nil, phraseEntry: nil,
            change: .passkeyRemoved(credentialId: credentialId))
        XCTAssertEqual(removed, .publish(entries: [try passwordEntry()], seq: 3))
        // Removing one that was never listed says nothing.
        XCTAssertEqual(
            SealAnnouncementPlan.decide(
                current: stands, highestSeq: 1, passwordEntry: nil, phraseEntry: nil,
                change: .passkeyRemoved(credentialId: Data([9]))),
            .nothing)
    }

    func testAtMostEightPasskeySeals() throws {
        var entries = [try passwordEntry()]
        for index in 0..<SealAnnouncementPlan.maxPasskeySeals {
            entries.append(try passkeyEntry(Data([UInt8(index), 0xEE])))
        }
        let outcome = SealAnnouncementPlan.decide(
            current: announcement(entries, seq: 1), highestSeq: 1, passwordEntry: nil, phraseEntry: nil,
            change: .passkeyAdded(credentialId: Data([0xFF, 0xFF]), prfOutput: prf))
        XCTAssertEqual(outcome, .nothing, "a ninth is not listed: that passkey signs in and keeps nothing")
    }

    func testSavingThePhraseListsItAndARotationTakesTheOldOneOut() throws {
        let words = RecoveryMnemonic.generate()
        let stands = announcement([try passwordEntry()], seq: 1)
        let saved = SealAnnouncementPlan.decide(
            current: stands, highestSeq: 1, passwordEntry: nil, phraseEntry: try phraseEntry(words),
            change: .phraseSaved)
        XCTAssertEqual(saved, .publish(entries: [try passwordEntry(), try phraseEntry(words)], seq: 2))

        // ADR-0074 — a rotation: the first announcement of the new
        // generation carries the password and passkey seals of before, and
        // NOT the phrase of before (the new words are listed once saved).
        let carried = [try passwordEntry(otherExportKey), try passkeyEntry()]
        let rotated = SealAnnouncementPlan.decide(
            current: nil, highestSeq: 2, carried: carried, passwordEntry: try passwordEntry(),
            phraseEntry: try phraseEntry(RecoveryMnemonic.generate()), change: .rotated)
        guard case .publish(let entries, 3) = rotated else { return XCTFail("\(rotated)") }
        XCTAssertEqual(
            Set(entries), Set(carried),
            "the announced password seal is carried, not replaced by this device's export key")
        XCTAssertFalse(entries.contains { $0.kind == .phrase })
    }

    // MARK: - What the device reads back

    func testTheSealsOfAnAnnouncement() throws {
        let words = RecoveryMnemonic.generate()
        let seals = AccountSeals([try passwordEntry(), try passkeyEntry(), try phraseEntry(words)])
        XCTAssertTrue(seals.phraseSaved)
        XCTAssertTrue(seals.passkeyKeepsChats(credentialId: credentialId))
        XCTAssertEqual(seals.keptBy, .phrase)
        XCTAssertEqual(AccountSeals([try passwordEntry()]), AccountSeals.passwordOnly)

        let store = AccountSealsStore(defaults: defaults())
        XCTAssertNil(store.read(accountIdB64: "acct"), "never read on this device")
        store.write(seals, accountIdB64: "acct")
        XCTAssertEqual(store.read(accountIdB64: "acct"), seals)
        XCTAssertFalse(store.passwordOnlyNoticeClosed(accountIdB64: "acct"))
        store.closePasswordOnlyNotice(accountIdB64: "acct")
        XCTAssertTrue(store.passwordOnlyNoticeClosed(accountIdB64: "acct"))
        XCTAssertFalse(store.phraseSealOwed(accountIdB64: "acct"))
        store.setPhraseSealOwed(true, accountIdB64: "acct")
        XCTAssertTrue(store.phraseSealOwed(accountIdB64: "acct"))
        store.clear(accountIdB64: "acct")
        XCTAssertNil(store.read(accountIdB64: "acct"))
        XCTAssertFalse(store.passwordOnlyNoticeClosed(accountIdB64: "acct"))
        XCTAssertFalse(store.phraseSealOwed(accountIdB64: "acct"))
    }

    // MARK: - Would a reset made now be sealed (UX 15.1c)

    func testAResetIsSealedOnlyByAKeyTheAnnouncementLists() throws {
        let stands = announcement([try passwordEntry(), try passkeyEntry()], seq: 6, generation: 3)
        let password = ResetSealPreview.Candidate(
            key: try ResetSealKey.fromPasswordExportKey(exportKey: exportKey), credentialId: nil)
        let stale = ResetSealPreview.Candidate(
            key: try ResetSealKey.fromPasswordExportKey(exportKey: otherExportKey), credentialId: nil)
        let passkey = ResetSealPreview.Candidate(
            key: try ResetSealKey.fromPasskeyPrf(prfOutput: prf), credentialId: credentialId)

        let sealed = try XCTUnwrap(ResetSealPreview.sealed(announcement: stands, candidates: [password]))
        XCTAssertEqual(sealed.announcementSeq, 6)
        XCTAssertEqual(sealed.fromGeneration, 3)
        XCTAssertEqual(sealed.key.kind(), .password)
        // A password changed by a mailed code on a device without the keys
        // is not the one announced: unsealed.
        XCTAssertNil(ResetSealPreview.sealed(announcement: stands, candidates: [stale]))
        // ...unless a passkey that keeps the chats was the factor used.
        XCTAssertEqual(
            ResetSealPreview.sealed(announcement: stands, candidates: [stale, passkey])?.key.kind(), .passkey)
        // The passkey's secret under another credential id is not listed.
        XCTAssertNil(ResetSealPreview.sealed(
            announcement: stands,
            candidates: [.init(key: try ResetSealKey.fromPasskeyPrf(prfOutput: prf), credentialId: Data([9]))]))
        XCTAssertNil(ResetSealPreview.sealed(announcement: nil, candidates: [password]), "no announcement at all")
        XCTAssertTrue(ResetSealPreview.listsPasskey(stands))
        XCTAssertFalse(ResetSealPreview.listsPasskey(announcement([try passwordEntry()])))
        XCTAssertFalse(ResetSealPreview.listsPasskey(nil))
    }

    func testTheLineBeforeTheWord() {
        XCTAssertEqual(ResetSealLine.decide(sealed: true, announcesPasskey: true, passkeysSupported: true), .sealed)
        XCTAssertEqual(
            ResetSealLine.decide(sealed: false, announcesPasskey: false, passkeysSupported: true),
            .unsealed(offersPasskey: false))
        XCTAssertEqual(
            ResetSealLine.decide(sealed: false, announcesPasskey: true, passkeysSupported: true),
            .unsealed(offersPasskey: true))
        XCTAssertEqual(
            ResetSealLine.decide(sealed: false, announcesPasskey: true, passkeysSupported: false),
            .unsealed(offersPasskey: false), "a passkey this device cannot use is not offered")
        for key in [ResetSealLine.sealedKey, ResetSealLine.unsealedKey, ResetSealLine.unsealedUsePasskeyKey] {
            XCTAssertNotEqual(Localization.text(key), key, key)
        }
    }

    // MARK: - The landing

    @MainActor
    func testALandingReplacesThePasswordSealOnlyWithAnExportKeyMadeByThePasswordInThisRun() throws {
        XCTAssertEqual(LandingSealChange.decide(exportKeyFresh: true), .password)
        XCTAssertEqual(LandingSealChange.decide(exportKeyFresh: false), .landing)

        // Alignment item N: only a key a password produced in this run; the
        // Keychain's cached copy never counts.
        RunExportKey.clearAll()
        defer { RunExportKey.clearAll() }
        let key = Data(repeating: 0x42, count: 64)
        XCTAssertNil(RunExportKey.get(accountIdB64: "acct"))
        XCTAssertFalse(RunExportKey.isPending(accountIdB64: "acct"))
        RunExportKey.note(accountIdB64: "acct", exportKey: Data([1, 2]))
        XCTAssertNil(RunExportKey.get(accountIdB64: "acct"), "not an export key")
        RunExportKey.note(accountIdB64: "acct", exportKey: key)
        XCTAssertEqual(RunExportKey.get(accountIdB64: "acct"), key)
        XCTAssertTrue(RunExportKey.isPending(accountIdB64: "acct"))
        XCTAssertNil(RunExportKey.get(accountIdB64: "other"), "per account")
        RunExportKey.taken(accountIdB64: "acct")
        XCTAssertFalse(RunExportKey.isPending(accountIdB64: "acct"))
        XCTAssertEqual(RunExportKey.get(accountIdB64: "acct"), key, "kept for later changes in the run")
        RunExportKey.clear(accountIdB64: "acct")
        XCTAssertNil(RunExportKey.get(accountIdB64: "acct"))
    }

    // MARK: - The publish, and a contact's check of what was published

    private final class Directory {
        var bundle: IdentityFetchResponse?
        var fetchError: Error?
        var publishErrors: [Error] = []
        var published: [Data] = []
        /// What "another device" publishes between this device's read and
        /// its write (the stale case).
        var beforePublish: (() -> Void)?
    }

    private func announcer(_ directory: Directory, name: String = #function) -> SealAnnouncer {
        SealAnnouncer(
            ports: .init(
                fetchOwnBundle: { _, _ in
                    if let error = directory.fetchError { throw error }
                    return directory.bundle
                },
                publish: { _, _, announcement, _ in
                    directory.beforePublish?()
                    directory.beforePublish = nil
                    if !directory.publishErrors.isEmpty { throw directory.publishErrors.removeFirst() }
                    directory.published.append(announcement)
                    if let bundle = directory.bundle {
                        directory.bundle = IdentityFetchResponse(
                            signaturePubkey: bundle.signaturePubkey, x25519Pubkey: bundle.x25519Pubkey,
                            supersessionProof: bundle.supersessionProof, keyGeneration: bundle.keyGeneration,
                            resetSeals: announcement.base64EncodedString(),
                            previousResetSeals: bundle.previousResetSeals)
                    }
                },
                nowMs: { 1_700_000_000_000 }),
            store: AccountSealsStore(defaults: defaults(name)))
    }

    private func bundle(
        _ identity: Identity, generation: UInt64 = 1, seals: Data? = nil, previous: Data? = nil,
        proof: String? = nil
    ) -> IdentityFetchResponse {
        IdentityFetchResponse(
            signaturePubkey: identity.signaturePublicKey().base64EncodedString(),
            x25519Pubkey: identity.x25519PublicKey().base64EncodedString(),
            supersessionProof: proof, keyGeneration: generation,
            resetSeals: seals?.base64EncodedString(), previousResetSeals: previous?.base64EncodedString())
    }

    private func ensure(
        _ announcer: SealAnnouncer, _ identity: Identity, generation: UInt64 = 1, exportKey: Data?,
        phrase: RecoveryMnemonic? = nil, change: SealChange
    ) async -> SealAnnouncer.Result {
        await announcer.ensure(
            accountId: accountId, accountIdB64: accountIdB64, identity: identity, keyGeneration: generation,
            sessionToken: "tok", exportKey: exportKey, phrase: phrase, change: change)
    }

    func testSignUpPublishesAnAnnouncementAContactsDeviceVerifiesAndASealedResetPassesItsCheck() async throws {
        let old = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let directory = Directory()
        directory.bundle = bundle(old)
        let announcer = announcer(directory)

        // At sign-up: the password alone.
        let first = await ensure(announcer, old, exportKey: exportKey, change: .password)
        XCTAssertEqual(first, .published(.passwordOnly))
        let published = try XCTUnwrap(directory.published.last)
        let opened = try openSealAnnouncement(announcement: published, accountKey: old.signaturePublicKey())
        XCTAssertEqual(opened.keyGeneration, 1)
        XCTAssertEqual(opened.seq, 1)
        XCTAssertEqual(opened.entries, [try passwordEntry()])
        // Another key does not open it.
        let stranger = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        XCTAssertThrowsError(
            try openSealAnnouncement(announcement: published, accountKey: stranger.signaturePublicKey()))

        // A landing afterwards reads it and publishes nothing.
        let landing = await ensure(announcer, old, exportKey: exportKey, change: .landing)
        XCTAssertEqual(landing, .unchanged(.passwordOnly))
        XCTAssertEqual(directory.published.count, 1)

        // The reset: what was just proven (the password) signs the new key
        // under that announcement...
        let sealed = try XCTUnwrap(ResetSealPreview.sealed(
            announcement: opened,
            candidates: [.init(key: try ResetSealKey.fromPasswordExportKey(exportKey: exportKey), credentialId: nil)]))
        let fresh = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let seal = try sealed.key.signResetSeal(
            accountId: accountId, announcementSeq: sealed.announcementSeq, fromGeneration: sealed.fromGeneration,
            newSignaturePubkey: fresh.signaturePublicKey())

        // ...and a contact's device, holding the OLD key, finds it valid.
        let check = try checkResetSeal(
            previousAnnouncement: published, resetSeal: seal, heldAccountKey: old.signaturePublicKey(),
            accountId: accountId, acceptedGeneration: 1, attestedGeneration: 2,
            fetchedKey: fresh.signaturePublicKey(), seqFloor: 0)
        XCTAssertTrue(check.valid, check.refusal ?? "")
        XCTAssertEqual(check.announcementSeq, 1)
        XCTAssertEqual(
            resetVerdict(ResetVerdictInput(
                heldKeyB64: old.signaturePublicKey().base64EncodedString(), acceptedGeneration: 1,
                fetchedKeyB64: fresh.signaturePublicKey().base64EncodedString(), bundleGeneration: 2,
                attest: .verified(keyGeneration: 2, signedAtMs: 5), log: LogCheck(status: .off, isReset: nil),
                seal: SealCheck(valid: check.valid, announcementSeq: check.announcementSeq))),
            .sealed(keyGeneration: 2, signedAtMs: 5, announcementSeq: 1))

        // A seal over ANOTHER new key (the server's own) is not this reset's.
        let forged = try checkResetSeal(
            previousAnnouncement: published, resetSeal: seal, heldAccountKey: old.signaturePublicKey(),
            accountId: accountId, acceptedGeneration: 1, attestedGeneration: 2,
            fetchedKey: stranger.signaturePublicKey(), seqFloor: 0)
        XCTAssertFalse(forged.valid)
        // A reset with no seal, and one by a password that was never
        // announced: unsealed at the contact.
        let bare = try checkResetSeal(
            previousAnnouncement: published, resetSeal: nil, heldAccountKey: old.signaturePublicKey(),
            accountId: accountId, acceptedGeneration: 1, attestedGeneration: 2,
            fetchedKey: fresh.signaturePublicKey(), seqFloor: 0)
        XCTAssertFalse(bare.valid)
        let wrongPassword = try ResetSealKey.fromPasswordExportKey(exportKey: otherExportKey).signResetSeal(
            accountId: accountId, announcementSeq: 1, fromGeneration: 1,
            newSignaturePubkey: fresh.signaturePublicKey())
        XCTAssertFalse(try checkResetSeal(
            previousAnnouncement: published, resetSeal: wrongPassword, heldAccountKey: old.signaturePublicKey(),
            accountId: accountId, acceptedGeneration: 1, attestedGeneration: 2,
            fetchedKey: fresh.signaturePublicKey(), seqFloor: 0).valid)
        // An announcement below the floor this device kept is an old one
        // served again.
        XCTAssertFalse(try checkResetSeal(
            previousAnnouncement: published, resetSeal: seal, heldAccountKey: old.signaturePublicKey(),
            accountId: accountId, acceptedGeneration: 1, attestedGeneration: 2,
            fetchedKey: fresh.signaturePublicKey(), seqFloor: 2).valid)
    }

    func testAPhraseAndAPasskeyAreAddedAndReadBackAndTheRecordIsKept() async throws {
        let identity = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let directory = Directory()
        directory.bundle = bundle(identity)
        let name = #function
        let announcer = announcer(directory, name: name)
        _ = await ensure(announcer, identity, exportKey: exportKey, change: .password)

        let words = RecoveryMnemonic.generate()
        let saved = await ensure(announcer, identity, exportKey: exportKey, phrase: words, change: .phraseSaved)
        XCTAssertEqual(saved, .published(AccountSeals(phraseSaved: true, passkeyTagsB64: [])))

        let added = await ensure(
            announcer, identity, exportKey: nil, change: .passkeyAdded(credentialId: credentialId, prfOutput: prf))
        guard case .published(let seals) = added else { return XCTFail("\(added)") }
        XCTAssertTrue(seals.phraseSaved, "what was listed stays listed")
        XCTAssertTrue(seals.passkeyKeepsChats(credentialId: credentialId))
        let last = try openSealAnnouncement(
            announcement: try XCTUnwrap(directory.published.last), accountKey: identity.signaturePublicKey())
        XCTAssertEqual(last.seq, 3)
        XCTAssertEqual(last.entries.count, 3)
        XCTAssertEqual(
            AccountSealsStore(defaults: UserDefaults(suiteName: "lakko.tests.reset-seals.\(name)")!)
                .read(accountIdB64: accountIdB64),
            seals, "written for the screens at every look")
    }

    func testAnotherDevicePublishingMeanwhileIsReadAgainOnce() async throws {
        let identity = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let directory = Directory()
        directory.bundle = bundle(identity)
        let announcer = announcer(directory)
        // Between the read and the write a sibling publishes seq 1.
        let sibling = try signSealAnnouncement(
            accountIdentity: identity, accountId: accountId, keyGeneration: 1, seq: 1, issuedAtMs: 1,
            entries: [try passwordEntry()])
        directory.beforePublish = { [self] in directory.bundle = self.bundle(identity, seals: sibling) }
        directory.publishErrors = [DiscoveryClientError.http(
            status: 409, endpoint: "/discovery/identity", body: #"{"error":"reset-seals-stale"}"#)]

        let result = await ensure(
            announcer, identity, exportKey: exportKey,
            change: .passkeyAdded(credentialId: credentialId, prfOutput: prf))
        guard case .published(let seals) = result else { return XCTFail("\(result)") }
        XCTAssertTrue(seals.passkeyKeepsChats(credentialId: credentialId))
        let last = try openSealAnnouncement(
            announcement: try XCTUnwrap(directory.published.last), accountKey: identity.signaturePublicKey())
        XCTAssertEqual(last.seq, 2, "above the sibling's")
        XCTAssertTrue(SealUploadRefusal.isStale(DiscoveryClientError.http(
            status: 409, endpoint: "/x", body: #"{"error":"reset-seals-stale"}"#)))
        XCTAssertTrue(SealUploadRefusal.isSealInvalid(DiscoveryClientError.http(
            status: 400, endpoint: "/x", body: #"{"error":"reset-seal-invalid"}"#)))
        XCTAssertFalse(SealUploadRefusal.isStale(URLError(.timedOut)))
    }

    func testNothingIsPublishedForAnotherKeyOrGenerationOrWhenTheDirectoryIsOut() async throws {
        let identity = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let other = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let directory = Directory()
        let announcer = announcer(directory)

        // The row holds another key (a reset's publish is still owed): an
        // announcement signed now would not be the row's.
        directory.bundle = bundle(other)
        var result = await ensure(announcer, identity, exportKey: exportKey, change: .password)
        XCTAssertEqual(result, .failed)
        // Another generation than this session's.
        directory.bundle = bundle(identity, generation: 2)
        result = await ensure(announcer, identity, generation: 1, exportKey: exportKey, change: .password)
        XCTAssertEqual(result, .failed)
        // No row, or no answer.
        directory.bundle = nil
        result = await ensure(announcer, identity, exportKey: exportKey, change: .password)
        XCTAssertEqual(result, .failed)
        directory.fetchError = URLError(.notConnectedToInternet)
        result = await ensure(announcer, identity, exportKey: exportKey, change: .password)
        XCTAssertEqual(result, .failed)
        XCTAssertTrue(directory.published.isEmpty)

        // A device with no export key on an account that announced nothing.
        directory.fetchError = nil
        directory.bundle = bundle(identity)
        result = await ensure(announcer, identity, exportKey: nil, change: .landing)
        XCTAssertEqual(result, .noPasswordSeal)
        XCTAssertTrue(directory.published.isEmpty)
    }

    /// ADR-0074 — after a rotation the new key signs, `seq` continues, and
    /// the password and passkey seals of before are carried.
    func testARotationCarriesThePasswordAndPasskeySealsToTheNewGeneration() async throws {
        let before = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let after = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let words = RecoveryMnemonic.generate()
        let previous = try signSealAnnouncement(
            accountIdentity: before, accountId: accountId, keyGeneration: 1, seq: 5, issuedAtMs: 1,
            entries: [try passwordEntry(), try passkeyEntry(), try phraseEntry(words)])
        let directory = Directory()
        directory.bundle = bundle(after, generation: 2, previous: previous, proof: "cHJvb2Y=")
        let announcer = announcer(directory)

        // A device that holds no export key (a passkey sign-in) still
        // carries the announced password seal.
        let result = await ensure(announcer, after, generation: 2, exportKey: nil, change: .rotated)
        guard case .published(let seals) = result else { return XCTFail("\(result)") }
        XCTAssertFalse(seals.phraseSaved, "the new words are not listed before they are confirmed")
        XCTAssertTrue(seals.passkeyKeepsChats(credentialId: credentialId))
        let opened = try openSealAnnouncement(
            announcement: try XCTUnwrap(directory.published.last), accountKey: after.signaturePublicKey())
        XCTAssertEqual(opened.keyGeneration, 2)
        XCTAssertEqual(opened.seq, 6, "seq continues over the generation")
        XCTAssertEqual(Set(opened.entries), [try passwordEntry(), try passkeyEntry()])
    }

    /// After a RESET nothing is carried: the passkeys were deleted, and the
    /// password seal is the one just proven.
    func testAfterAResetTheFirstAnnouncementListsOnlyThePasswordJustProven() async throws {
        let before = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let after = try Identity.fromSeed(seed: RecoveryMnemonic.generate().toSeed())
        let previous = try signSealAnnouncement(
            accountIdentity: before, accountId: accountId, keyGeneration: 1, seq: 5, issuedAtMs: 1,
            entries: [try passwordEntry(otherExportKey), try passkeyEntry()])
        let directory = Directory()
        // An attested reset publishes no supersession proof.
        directory.bundle = bundle(after, generation: 2, previous: previous)
        let announcer = announcer(directory)

        let result = await ensure(announcer, after, generation: 2, exportKey: exportKey, change: .password)
        XCTAssertEqual(result, .published(.passwordOnly))
        let opened = try openSealAnnouncement(
            announcement: try XCTUnwrap(directory.published.last), accountKey: after.signaturePublicKey())
        XCTAssertEqual(opened.entries, [try passwordEntry()])
        XCTAssertEqual(opened.seq, 6)
    }
}
