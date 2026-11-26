// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0101 section 8 — profile v6 is signed by an account identity. The
/// identity tests sign with: a fixed one, never a real account's.
enum TestProfileSigning {
    // swiftlint:disable:next force_try
    static let identity: Identity = try! Identity.fromSeed(seed: Data(repeating: 0x6B, count: 32))
    /// The provider a test ``ProfileDirectory`` signs its own envelopes with.
    static let provider: @MainActor (String) -> Identity? = { _ in identity }
}

extension ProfileClaim {
    /// ADR-0101 section 8 — a received claim as the store keeps it: the
    /// decoded fields and the raw envelope (signed by
    /// ``TestProfileSigning/identity``), what a bundle or a backup carries.
    static func signed(
        displayName: String?, username: String? = nil, updatedAtMs: UInt64, accountId: Data? = nil
    ) -> ProfileClaim {
        // swiftlint:disable:next force_try
        let envelope = try! composeProfileEnvelope(profile: FfiProfile(
            displayName: displayName, username: username, updatedAtMs: updatedAtMs,
            accountId: accountId, usernameCertificate: nil, grants: []))
        return ProfileClaim(
            displayName: displayName, username: username, updatedAtMs: updatedAtMs, accountId: accountId,
            envelope: envelope)
    }
}

/// The suite's own `composeProfileEnvelope(profile:)`: the core's, signed by
/// ``TestProfileSigning/identity`` (the core takes the signer since profile v6).
func composeProfileEnvelope(profile: FfiProfile) throws -> Data {
    try LakkoCore.composeProfileEnvelope(profile: profile, accountIdentity: TestProfileSigning.identity)
}

/// ADR-0048 on iOS — the username rule as the forms apply it, the display-name
/// resolver's precedence, the received-profile store's ordering and keying,
/// the username verification state machine, the own-profile stamp, and the
/// receive-path classification of a profile envelope.
///
/// Where the core owns a rule (`validateUsername`, `normalizeDisplayName`,
/// `profileIsNewer`, `composeProfileEnvelope` / `decodeProfileEnvelope`) the
/// tests call the REAL FFI, so a Swift restatement cannot drift unnoticed.
@MainActor
final class ProfileNamesTests: XCTestCase {

    private let keyA = Data(repeating: 0xA1, count: 32)
    private let keyB = Data(repeating: 0xB2, count: 32)
    private var keyAB64: String { base64Encode(keyA) }
    private var keyBB64: String { base64Encode(keyB) }

    // MARK: - Username rule (UI decisions)

    func testTheRealCoreRuleMapsToTheSpecReasons() {
        XCTAssertEqual(checkUsername("@Alice"), .valid(canonical: "alice"))
        XCTAssertEqual(checkUsername("  bob_42 "), .valid(canonical: "bob_42"))
        XCTAssertEqual(checkUsername(""), .empty)
        XCTAssertEqual(checkUsername("@"), .empty)
        XCTAssertEqual(checkUsername("al"), .invalid(.tooShort))
        XCTAssertEqual(checkUsername(String(repeating: "a", count: 33)), .invalid(.tooLong))
        // Cyrillic `а` in `pаvel` — the look-alike the rule exists to refuse.
        XCTAssertEqual(checkUsername("p\u{0430}vel"), .invalid(.invalidCharacters))
        XCTAssertEqual(checkUsername("ali-ce"), .invalid(.invalidCharacters))
    }

    func testReasonCopyIsTheSpecCopy() {
        XCTAssertEqual(UsernameRuleReason.invalidCharacters.message, "Only Latin letters, digits and _")
        XCTAssertEqual(UsernameRuleReason.tooShort.message, "At least 3 characters")
        XCTAssertEqual(UsernameRuleReason.tooLong.message, "At most 32 characters")
    }

    func testEachTypedVariantMapsToItsReason() {
        func check(_ error: FfiError) -> UsernameCheck {
            checkUsername("x", validate: { _ in throw error })
        }
        XCTAssertEqual(check(.UsernameEmpty(message: "")), .empty)
        XCTAssertEqual(check(.UsernameTooShort(message: "")), .invalid(.tooShort))
        XCTAssertEqual(check(.UsernameTooLong(message: "")), .invalid(.tooLong))
        XCTAssertEqual(check(.UsernameInvalidCharacters(message: "")), .invalid(.invalidCharacters))
        // Fail-closed: an unexpected refusal never allows a lookup.
        XCTAssertFalse(check(.Discovery(message: "boom")).allowsLookup)
    }

    func testOnlyAValidNameAllowsALookupAndOnlyAnInvalidOneShowsAReason() {
        XCTAssertTrue(UsernameCheck.valid(canonical: "alice").allowsLookup)
        XCTAssertFalse(UsernameCheck.empty.allowsLookup)
        XCTAssertFalse(UsernameCheck.invalid(.tooShort).allowsLookup)
        XCTAssertNil(UsernameCheck.empty.inlineMessage)
        XCTAssertNil(UsernameCheck.valid(canonical: "alice").inlineMessage)
        XCTAssertEqual(UsernameCheck.invalid(.tooLong).inlineMessage, "At most 32 characters")
    }

    func testLoginRefusesAnInvalidNameBeforeDerivingAHandle() {
        var derived = 0
        do {
            _ = try LoginName.make(
                "p\u{0430}vel",
                emptyError: AuthClientError.argument("empty"),
                derive: { _ in
                    derived += 1
                    return Data(count: 32)
                }
            )
            XCTFail("an invalid username must not produce a handle")
        } catch let violation as UsernameRuleViolation {
            XCTAssertEqual(violation.reason, .invalidCharacters)
        } catch {
            XCTFail("expected a UsernameRuleViolation, got \(error)")
        }
        XCTAssertEqual(derived, 0)
    }

    // MARK: - Resolver precedence

    private func resolve(
        rename: String? = nil,
        name: String? = nil,
        username: String? = nil,
        verification: UsernameVerificationStateView? = nil
    ) -> ResolvedDisplayName {
        resolveDisplayName(
            signatureKey: Data([0xa1, 0xb2, 0xc3, 0xd4, 0xe5]),
            localRename: rename,
            profile: (name == nil && username == nil)
                ? nil : ProfileClaim(displayName: name, username: username, updatedAtMs: 1),
            verification: verification
        )
    }

    func testPrecedenceTable() {
        // rename > profile name > verified username > neutral placeholder
        XCTAssertEqual(resolve(rename: "Mum", name: "Alice", username: "alice", verification: .verified).name, "Mum")
        XCTAssertEqual(resolve(name: "Alice", username: "alice", verification: .verified).name, "Alice")
        XCTAssertEqual(resolve(username: "alice", verification: .verified).name, "@alice")
        XCTAssertEqual(resolve().name, "Member")
        XCTAssertEqual(resolve().source, .placeholder)
        XCTAssertTrue(resolve().isPlaceholder)
        XCTAssertFalse(resolve(name: "Alice").isPlaceholder)
    }

    func testTheFingerprintIsNeverANameButStaysReachable() {
        // UX spec section 2, principle 4: no hex key as a label anywhere.
        let unknown = resolve()
        XCTAssertEqual(unknown.name, "Member")
        XCTAssertEqual(unknown.listLabel, "Member")
        XCTAssertEqual(unknown.requestSenderLabel, "Someone")
        XCTAssertEqual(unknown.fingerprint, "a1b2 c3d4", "the safety-number surface still gets the key")
        // A person with a name never gets the request placeholder.
        XCTAssertEqual(resolve(name: "Alice").requestSenderLabel, "Alice")
        XCTAssertEqual(resolve(username: "alice", verification: .verified).requestSenderLabel, "@alice")
    }

    func testAnUnverifiedUsernameIsNeverShown() {
        for verification: UsernameVerificationStateView? in [nil, .refused] {
            let r = resolve(username: "pavel", verification: verification)
            XCTAssertEqual(r.name, "Member", "an unverified claim falls through to the placeholder")
            XCTAssertNil(r.verifiedUsername)
            XCTAssertNil(r.usernameLine)
            XCTAssertFalse(r.listLabel.contains("@"))
        }
        XCTAssertFalse(resolve(username: "pavel", verification: nil).usernameNotVerified,
                       "still being verified is not a failure")
        XCTAssertTrue(resolve(username: "pavel", verification: .refused).usernameNotVerified)
    }

    func testTheFingerprintNeverCarriesAnAt() {
        XCTAssertFalse(resolve().fingerprint.contains("@"))
        XCTAssertEqual(displayFingerprint(Data([0x00, 0x0f, 0xf0, 0xff, 0x12])), "000f f0ff")
    }

    func testListLabelAndSecondLines() {
        let named = resolve(name: "Alice Petrova", username: "alice", verification: .verified)
        XCTAssertEqual(named.listLabel, "Alice Petrova · @alice")
        XCTAssertEqual(named.usernameLine, "@alice")
        let usernameOnly = resolve(username: "alice", verification: .verified)
        XCTAssertEqual(usernameOnly.listLabel, "@alice", "the name already is the username")
        XCTAssertNil(usernameOnly.usernameLine)
        let renamed = resolve(rename: "Mum", name: "Alice Petrova")
        XCTAssertEqual(renamed.theirName, "Alice Petrova")
        XCTAssertNil(resolve(name: "Alice Petrova").theirName, "no rename, no 'Their name' line")
        XCTAssertEqual(resolve(rename: "  ", name: "Alice").name, "Alice", "a blank rename is no rename")
    }

    // MARK: - Received profile store

    private final class MemoryPersistence: ProfilePersistence {
        var blobs: [String: Data] = [:]
        func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
        func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
        func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
    }

    private func makeStore(_ persistence: MemoryPersistence = MemoryPersistence()) -> ReceivedProfileStore {
        let store = ReceivedProfileStore(
            persistence: persistence,
            isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }
        )
        store.activate(accountIdB64: "acct")
        return store
    }

    private func claim(_ name: String?, at ms: UInt64, username: String? = nil) -> ProfileClaim {
        ProfileClaim(displayName: name, username: username, updatedAtMs: ms)
    }

    func testOnlyAStrictlyNewerProfileReplacesTheStoredOne() {
        let store = makeStore()
        XCTAssertEqual(store.accept(claim("Alice", at: 10), signatureKeyB64: keyAB64), .stored)
        XCTAssertEqual(store.accept(claim("Old", at: 9), signatureKeyB64: keyAB64), .notNewer)
        XCTAssertEqual(store.accept(claim("Same", at: 10), signatureKeyB64: keyAB64), .notNewer,
                       "equal is not newer: a re-broadcast is not an update")
        XCTAssertEqual(store.profile(signatureKeyB64: keyAB64)?.displayName, "Alice")
        XCTAssertEqual(store.accept(claim("Alice P", at: 11), signatureKeyB64: keyAB64), .stored)
        XCTAssertEqual(store.profile(signatureKeyB64: keyAB64)?.displayName, "Alice P")
    }

    func testProfilesAreKeyedPerSignatureKeyAndNeedOne() {
        let store = makeStore()
        XCTAssertEqual(store.accept(claim("Alice", at: 50), signatureKeyB64: keyAB64), .stored)
        XCTAssertEqual(store.accept(claim("Bob", at: 1), signatureKeyB64: keyBB64), .stored,
                       "another key's older stamp is irrelevant")
        XCTAssertEqual(store.profile(signatureKeyB64: keyAB64)?.displayName, "Alice")
        XCTAssertEqual(store.profile(signatureKeyB64: keyBB64)?.displayName, "Bob")
        XCTAssertEqual(store.accept(claim("X", at: 99), signatureKeyB64: nil), .noSender)
        XCTAssertEqual(store.accept(claim("X", at: 99), signatureKeyB64: AuthorIdentity.ownReactionSentinel), .noSender)
        XCTAssertEqual(store.accept(claim("X", at: 99), signatureKeyB64: base64Encode(Data([1, 2, 3]))), .noSender,
                       "a leaf rendered as bytes is not a signature key")
    }

    func testTheStoreSurvivesAReloadAndIsPerAccount() {
        let persistence = MemoryPersistence()
        let store = makeStore(persistence)
        _ = store.accept(claim("Alice", at: 10), signatureKeyB64: keyAB64)
        let reloaded = makeStore(persistence)
        XCTAssertEqual(reloaded.profile(signatureKeyB64: keyAB64)?.displayName, "Alice")
        XCTAssertEqual(reloaded.accept(claim("Stale", at: 10), signatureKeyB64: keyAB64), .notNewer,
                       "the order survives a relaunch")
        reloaded.activate(accountIdB64: "other")
        XCTAssertNil(reloaded.profile(signatureKeyB64: keyAB64))
        let inactive = ReceivedProfileStore(persistence: persistence, isNewer: { $0 > $1 })
        XCTAssertEqual(inactive.accept(claim("A", at: 1), signatureKeyB64: keyAB64), .inactive)
    }

    // MARK: - Own profile

    func testOwnProfileStampIsStrictlyIncreasing() throws {
        let suite = "lakko.tests.own-profile.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = OwnProfileStore(defaults: defaults)
        let first = store.save(displayName: "Alice", accountIdB64: "acct", nowMs: 1_000)
        XCTAssertEqual(first.updatedAtMs, 1_000)
        let sameMillisecond = store.save(displayName: "Alice P", accountIdB64: "acct", nowMs: 1_000)
        XCTAssertEqual(sameMillisecond.updatedAtMs, 1_001, "two saves in one ms still order")
        let clockStepBack = store.save(displayName: "A", accountIdB64: "acct", nowMs: 5)
        XCTAssertEqual(clockStepBack.updatedAtMs, 1_002, "a backward clock never un-orders a save")
        XCTAssertTrue(profileIsNewer(incomingUpdatedAtMs: clockStepBack.updatedAtMs,
                                     storedUpdatedAtMs: sameMillisecond.updatedAtMs))
        XCTAssertEqual(store.load(accountIdB64: "acct")?.displayName, "A")
    }

    func testDisplayNameRuleUsesTheCoreAndItsCopy() {
        XCTAssertEqual(try checkDisplayName("  Alice Petrova  "), "Alice Petrova")
        XCTAssertNil(try checkDisplayName("   "), "blank is no name")
        XCTAssertThrowsError(try checkDisplayName("Alice\nPetrova")) {
            XCTAssertEqual(($0 as? DisplayNameRuleViolation)?.message, "One line only")
        }
        XCTAssertThrowsError(try checkDisplayName("Alice\u{202E}")) {
            XCTAssertEqual(
                ($0 as? DisplayNameRuleViolation)?.message,
                "Remove invisible or text-direction characters")
        }
        XCTAssertThrowsError(try checkDisplayName(String(repeating: "a", count: 65))) {
            XCTAssertEqual(($0 as? DisplayNameRuleViolation)?.message, "At most 64 characters")
        }
    }

    // MARK: - Username verification

    private final class ResolveLog {
        var calls: [String] = []
    }

    private func makeVerifier(
        now: @escaping () -> Int64 = { 1_000 },
        defaults: UserDefaults? = nil,
        resolve: @escaping (String) async throws -> UsernameResolution
    ) -> UsernameVerifier {
        let verifier = UsernameVerifier(
            resolve: resolve, nowMs: now, retryAfterMs: 60_000, defaults: defaults, autoRetry: false)
        verifier.activate(accountIdB64: "acct")
        return verifier
    }

    func testVerifiedOnlyWhenTheResolvedKeyIsTheSendersKey() async {
        let keyA = self.keyA
        let verifier = makeVerifier { username in
            username == "alice" ? .signatureKey(keyA) : .notRegistered
        }
        let verified = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        XCTAssertEqual(verified, .verified)
        let impersonation = await verifier.verify(signatureKeyB64: keyBB64, username: "alice")
        XCTAssertEqual(impersonation, .mismatch, "@alice resolves to A, so B claiming it is refused")
        let unregistered = await verifier.verify(signatureKeyB64: keyAB64, username: "nobody")
        XCTAssertEqual(unregistered, .mismatch)
    }

    func testAChangedClaimIsVerifiedAgainAndTheOldVerdictDoesNotCarryOver() async {
        let log = ResolveLog()
        let keyA = self.keyA
        let verifier = makeVerifier { username in
            log.calls.append(username)
            return username == "alice" ? .signatureKey(keyA) : .notRegistered
        }
        _ = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        _ = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        XCTAssertEqual(log.calls, ["alice"], "a decided pair is answered from the cache")
        let changed = await verifier.verify(signatureKeyB64: keyAB64, username: "alice2")
        XCTAssertEqual(changed, .mismatch)
        XCTAssertEqual(log.calls, ["alice", "alice2"])
        XCTAssertEqual(verifier.state(signatureKeyB64: keyAB64, username: "alice"), .verified)
        XCTAssertEqual(verifier.state(signatureKeyB64: keyAB64, username: "alice2"), .mismatch)
    }

    func testConcurrentRequestsForOnePairShareOneLookup() async {
        let log = ResolveLog()
        let keyA = self.keyA
        let verifier = makeVerifier { username in
            log.calls.append(username)
            try await Task.sleep(nanoseconds: 50_000_000)
            return .signatureKey(keyA)
        }
        let key = keyAB64
        async let first = verifier.verify(signatureKeyB64: key, username: "alice")
        async let second = verifier.verify(signatureKeyB64: key, username: "alice")
        async let third = verifier.verify(signatureKeyB64: key, username: "alice")
        let results = await [first, second, third]
        XCTAssertEqual(results, [.verified, .verified, .verified])
        XCTAssertEqual(log.calls.count, 1, "single-flight: one proof-of-work lookup for one pair")
    }

    func testATransientFailureIsRetriedAfterAWaitThatStartsAtSecondsAndGrows() async {
        var now: Int64 = 1_000
        var fail = true
        let log = ResolveLog()
        let keyA = self.keyA
        let verifier = makeVerifier(now: { now }) { username in
            log.calls.append(username)
            if fail { throw URLError(.notConnectedToInternet) }
            return .signatureKey(keyA)
        }
        let failed = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        XCTAssertEqual(failed, .failed(atMs: 1_000))
        XCTAssertFalse(verifier.needsVerification(signatureKeyB64: keyAB64, username: "alice"))
        // The first wait is 15 s.
        now = 10_000
        let stillFailed = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        XCTAssertEqual(stillFailed, .failed(atMs: 1_000))
        XCTAssertEqual(log.calls.count, 1)
        now = 16_000
        XCTAssertTrue(verifier.needsVerification(signatureKeyB64: keyAB64, username: "alice"))
        let failedAgain = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        XCTAssertEqual(failedAgain, .failed(atMs: 16_000))
        XCTAssertEqual(log.calls.count, 2)
        // The second wait is 30 s.
        now = 31_000
        XCTAssertFalse(verifier.needsVerification(signatureKeyB64: keyAB64, username: "alice"))
        now = 46_000
        fail = false
        let retried = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        XCTAssertEqual(retried, .verified)
        XCTAssertEqual(log.calls.count, 3)
    }

    func testTheRetryWaitsGrowToTheirCap() {
        XCTAssertEqual(UsernameVerifier.retryDelayMs(failures: 1, cap: 300_000), 15_000)
        XCTAssertEqual(UsernameVerifier.retryDelayMs(failures: 2, cap: 300_000), 30_000)
        XCTAssertEqual(UsernameVerifier.retryDelayMs(failures: 50, cap: 300_000), 300_000)
        XCTAssertEqual(UsernameVerifier.retryDelayMs(failures: 50, cap: 60_000), 60_000)
    }

    func testACheckThatCouldNotCompleteIsNotFlaggedAsNotVerified() {
        XCTAssertNil(UsernameVerificationStateView(.failed(atMs: 1)))
        XCTAssertNil(UsernameVerificationStateView(nil))
        XCTAssertEqual(UsernameVerificationStateView(.mismatch), .refused)
        XCTAssertFalse(UsernameVerificationState.failed(atMs: 1).isRefusal)
        XCTAssertFalse(resolve(username: "pavel", verification: nil).usernameNotVerified)
    }

    func testDefinitiveVerdictsPersistAndFailuresDoNot() async throws {
        let suite = "lakko.tests.verdicts.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let keyA = self.keyA
        let first = makeVerifier(defaults: defaults) { username in
            if username == "flaky" { throw URLError(.timedOut) }
            return .signatureKey(keyA)
        }
        _ = await first.verify(signatureKeyB64: keyAB64, username: "alice")
        _ = await first.verify(signatureKeyB64: keyAB64, username: "flaky")
        let second = makeVerifier(defaults: defaults) { _ in
            XCTFail("a persisted verdict must not look up again")
            return .notRegistered
        }
        XCTAssertEqual(second.state(signatureKeyB64: keyAB64, username: "alice"), .verified)
        XCTAssertNil(second.state(signatureKeyB64: keyAB64, username: "flaky"))
    }

    // MARK: - Receive-path classification

    private func classify(_ plaintext: Data, senderLeaf: UInt32? = 3) -> IncomingChatEvent {
        ChatClient.toEvent(
            messageId: 9,
            createdAt: "2026-09-13T00:00:00Z",
            processed: .application(data: plaintext, senderLeaf: senderLeaf, senderAccount: nil),
            resolveSender: { "member-\($0)" }
        )
    }

    func testAProfileEnvelopeIsClassifiedWithTheResolvedSenderNeverAsChat() throws {
        // Envelope v2 — the sender's account id rides along and is kept on
        // the claim (what a group-call tile is matched on, ADR-0021).
        let accountId = Data(repeating: 0xAC, count: 16)
        let bytes = try composeProfileEnvelope(
            profile: FfiProfile(
                displayName: "Alice Petrova", username: "alice", updatedAtMs: 42, accountId: accountId,
                usernameCertificate: nil, grants: []))
        let event = classify(bytes)
        guard case .profile(let messageId, let profile, let senderKey, _) = event else {
            return XCTFail("a profile envelope must classify as .profile, got \(event)")
        }
        XCTAssertEqual(messageId, 9)
        XCTAssertEqual(
            profile,
            ProfileClaim(
                displayName: "Alice Petrova", username: "alice", updatedAtMs: 42, accountId: accountId,
                // ADR-0101 section 8 — the envelope rides along verbatim.
                envelope: bytes))
        XCTAssertEqual(senderKey, "member-3", "bound to the identity the sender leaf names at receipt")
        guard case .profile(_, _, .none, _) = classify(bytes, senderLeaf: nil) else {
            return XCTFail("an unattributable profile still never becomes a bubble")
        }
    }

    func testWithoutTheArmTheProfileWouldBeDroppedUnread() throws {
        let bytes = try composeProfileEnvelope(
            profile: FfiProfile(
                displayName: "Alice", username: nil, updatedAtMs: 1, accountId: nil,
                usernameCertificate: nil, grants: []))
        let parsed = parseBodyEnvelope(bytes: bytes)
        // Since 2026-09-29 the parser drops any `lakko/` plaintext as a service
        // envelope (it used to degrade it to a chat body reading
        // `lakko/profile/v1` plus binary). Without the arm the envelope would
        // therefore never render, and never reach its receive handler either.
        XCTAssertEqual(parsed.kind, .serviceEnvelope)
        XCTAssertEqual(parsed.text, "")
    }

    func testAChatEnvelopeStillClassifiesAsChat() throws {
        let event = classify(try composeEnvelope(text: "lunch?", attachments: []))
        guard case .application(_, let text, _, _, _, _, _) = event else {
            return XCTFail("a chat message must stay a chat message, got \(event)")
        }
        XCTAssertEqual(text, "lunch?")
    }

    // MARK: - Growth detection

    func testGrowthIsASetDifferenceOverKeys() {
        XCTAssertTrue(membershipGrew(previousMemberKeys: ["a"], currentMemberKeys: ["a", "b"]))
        XCTAssertTrue(membershipGrew(previousMemberKeys: ["a", "b"], currentMemberKeys: ["a", "c"]),
                      "remove + add in one batch keeps the count and is still growth")
        XCTAssertFalse(membershipGrew(previousMemberKeys: ["a", "b"], currentMemberKeys: ["a"]))
        XCTAssertFalse(membershipGrew(previousMemberKeys: ["a"], currentMemberKeys: ["a"]))
        XCTAssertFalse(membershipGrew(previousMemberKeys: nil, currentMemberKeys: ["a", "b"]))
        XCTAssertFalse(membershipGrew(previousMemberKeys: ["a"], currentMemberKeys: nil))
    }

    // MARK: - ADR-0092 section 7 — "my card may be shared"

    func testTheOwnFlagIsOffExactlyOnNobody() {
        let base = defaultVisibilitySetting()
        XCTAssertFalse(ProfileDirectory.cardShareable(for: chooseVisibilityMode(setting: base, mode: .nobody)))
        XCTAssertTrue(ProfileDirectory.cardShareable(for: chooseVisibilityMode(setting: base, mode: .everyone)))
        XCTAssertTrue(ProfileDirectory.cardShareable(for: chooseVisibilityMode(setting: base, mode: .some)))
    }

    func testTheSharingFlagTravelsInTheEnvelopeAndIsKeptOnTheClaim() throws {
        for shareable in [true, false] {
            let bytes = try composeProfileEnvelope(profile: FfiProfile(
                displayName: "Anna", username: nil, updatedAtMs: 5, accountId: nil,
                usernameCertificate: nil, grants: [], cardShareable: shareable))
            XCTAssertEqual(try decodeProfileEnvelope(plaintext: bytes).cardShareable, shareable)
            XCTAssertEqual(ChatClient.decodeProfileIfEnvelope(bytes)?.cardShareable, shareable)
        }
    }

    func testAStoredClaimFromBeforeTheFieldReadsAsShareable() throws {
        let old = Data(#"{"displayName":"Anna","updatedAtMs":3}"#.utf8)
        XCTAssertTrue(try JSONDecoder().decode(ProfileClaim.self, from: old).cardShareable)
        let kept = ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 3, cardShareable: false)
        let again = try JSONDecoder().decode(ProfileClaim.self, from: JSONEncoder().encode(kept))
        XCTAssertFalse(again.cardShareable)
        XCTAssertFalse(kept.withoutGrants.cardShareable)
    }

    func testANewerProfileCarriesTheNewFlagAndAnEqualOneDoesNot() {
        let store = makeStore()
        XCTAssertEqual(store.accept(claim("Anna", at: 10), signatureKeyB64: keyAB64), .stored)
        let off = ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 10, cardShareable: false)
        XCTAssertEqual(store.accept(off, signatureKeyB64: keyAB64), .notNewer,
                       "the same stamp is not an update: the sender re-stamps on a change")
        let offLater = ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 11, cardShareable: false)
        XCTAssertEqual(store.accept(offLater, signatureKeyB64: keyAB64), .stored)
        XCTAssertEqual(store.profile(signatureKeyB64: keyAB64)?.cardShareable, false)
    }

    func testANamelessProfileKeepsAFoundNameAndTakesOnlyTheFlag() {
        let store = makeStore()
        XCTAssertEqual(store.accept(claim("Found", at: 0, username: "anna"), signatureKeyB64: keyAB64), .stored)
        let nameless = ProfileClaim(
            displayName: nil, username: nil, updatedAtMs: 20, accountId: Data(repeating: 1, count: 16),
            cardShareable: false)
        XCTAssertEqual(store.accept(nameless, signatureKeyB64: keyAB64), .stored)
        let held = store.profile(signatureKeyB64: keyAB64)
        XCTAssertEqual(held?.displayName, "Found")
        XCTAssertEqual(held?.username, "anna")
        XCTAssertEqual(held?.cardShareable, false)
        XCTAssertEqual(held?.isFoundName, true, "a later profile with a name still replaces it")
    }

    func testThePickerDisablesAContactWhoDoesNotLetTheirCardBeShared() {
        let key = Data(repeating: 2, count: 32)
        func row(username: String?, shareable: Bool) -> ContactCardCandidate {
            ContactCardCandidate(id: "k", name: "Anna", username: username, accountKey: key, shareable: shareable)
        }
        XCTAssertEqual(row(username: "anna", shareable: true).availability, .available)
        XCTAssertEqual(row(username: "anna", shareable: false).availability, .notShareable)
        XCTAssertEqual(row(username: nil, shareable: false).availability, .notShareable)
        XCTAssertEqual(row(username: nil, shareable: true).availability, .noUsername)
        XCTAssertEqual(
            Localization.text("contact.not_shareable", texts: ["name": "Anna"]),
            "\u{2068}Anna\u{2069} doesn't let others share their contact",
            "the name is isolated like every text argument")
    }
}

/// ADR-0048 §2 — the publish triggers: join and change publish now, growth
/// republishes throttled and jittered per group.
@MainActor
final class ProfilePublisherTests: XCTestCase {

    private final class Harness {
        var sent: [(group: String, bytes: Data)] = []
        var sleeps: [Int64] = []
        var now: Int64 = 100_000
        var jitter: Int64 = 0
        var envelope: Data? = Data([0x6C, 0x01])
        var blocked: Set<String> = []
        var failSends = false
        var heldSleeps: [CheckedContinuation<Void, Never>] = []

        /// Wait until `count` sleeps are parked, bounded so a broken
        /// implementation fails instead of hanging.
        func waitForHeldSleeps(_ count: Int) async {
            for _ in 0..<200 where heldSleeps.count < count {
                try? await Task.sleep(nanoseconds: 5_000_000)
            }
        }

        func releaseSleeps() {
            let held = heldSleeps
            heldSleeps = []
            held.forEach { $0.resume() }
        }

        func ports(holdSleeps: Bool = false) -> ProfilePublisher.Ports {
            ProfilePublisher.Ports(
                send: { [unowned self] group, bytes in
                    if self.failSends { throw URLError(.notConnectedToInternet) }
                    self.sent.append((group, bytes))
                },
                envelope: { [unowned self] _ in self.envelope },
                canSend: { [unowned self] in !self.blocked.contains($0) },
                nowMs: { [unowned self] in self.now },
                jitterMs: { [unowned self] in self.jitter },
                sleep: { [unowned self] ms in
                    self.sleeps.append(ms)
                    if holdSleeps {
                        await withCheckedContinuation { continuation in
                            self.heldSleeps.append(continuation)
                        }
                    }
                }
            )
        }
    }

    /// An owed profile (growth seen) goes out at once before this device's
    /// next message, with no jitter, and the scheduled republish does not
    /// send a second copy.
    func testAnOwedProfileGoesOutBeforeTheNextMessageWithNoJitter() async {
        let h = Harness()
        h.jitter = 9_000
        let publisher = ProfilePublisher(ports: h.ports(holdSleeps: true))
        let scheduled = publisher.noteGrowth(groupIdHex: "g1")
        await h.waitForHeldSleeps(1)
        XCTAssertTrue(publisher.isOwed(groupIdHex: "g1"))
        await publisher.publishOwedBeforeSend(groupIdHex: "g1")
        XCTAssertEqual(h.sent.map(\.group), ["g1"], "the profile went out before the message")
        XCTAssertFalse(publisher.isOwed(groupIdHex: "g1"))
        h.releaseSleeps()
        await scheduled?.value
        XCTAssertEqual(h.sent.map(\.group), ["g1"], "the jittered republish did not send again")
    }

    /// A failed publish is owed; the next message carries it first.
    func testAFailedJoinPublishIsOwedToTheNextMessage() async {
        let h = Harness()
        let publisher = ProfilePublisher(ports: h.ports())
        h.failSends = true
        await publisher.publishNow(groupIdHex: "g1")
        XCTAssertTrue(publisher.isOwed(groupIdHex: "g1"))
        h.failSends = false
        await publisher.publishOwedBeforeSend(groupIdHex: "g1")
        XCTAssertEqual(h.sent.map(\.group), ["g1"])
        XCTAssertTrue(h.sleeps.isEmpty, "an owed profile is never delayed")
    }

    /// Nothing owed, nothing sent ahead of a message.
    func testNothingOwedSendsNothingAheadOfAMessage() async {
        let h = Harness()
        let publisher = ProfilePublisher(ports: h.ports())
        await publisher.publishNow(groupIdHex: "g1")
        await publisher.publishOwedBeforeSend(groupIdHex: "g1")
        await publisher.publishOwedBeforeSend(groupIdHex: "g2")
        XCTAssertEqual(h.sent.map(\.group), ["g1"])
    }

    func testJoinAndChangePublishNow() async {
        let h = Harness()
        let publisher = ProfilePublisher(ports: h.ports())
        await publisher.publishNow(groupIdHex: "g1")
        await publisher.publishToAll(groupIdHexes: ["g1", "g2", "g3"])
        XCTAssertEqual(h.sent.map(\.group), ["g1", "g1", "g2", "g3"])
        XCTAssertTrue(h.sleeps.isEmpty, "join and change are never delayed")
    }

    func testNothingIsSentIntoAGroupThisDeviceCannotSendToOrWithoutAProfile() async {
        let h = Harness()
        h.blocked = ["evicted"]
        let publisher = ProfilePublisher(ports: h.ports())
        await publisher.publishNow(groupIdHex: "evicted")
        h.envelope = nil
        await publisher.publishNow(groupIdHex: "g1")
        XCTAssertTrue(h.sent.isEmpty)
    }

    func testGrowthWithNoRecentPublishWaitsOnlyTheJitter() async {
        let h = Harness()
        h.jitter = 7_000
        let publisher = ProfilePublisher(ports: h.ports())
        await publisher.noteGrowth(groupIdHex: "g1")?.value
        XCTAssertEqual(h.sleeps, [7_000])
        XCTAssertEqual(h.sent.map(\.group), ["g1"])
    }

    func testGrowthSoonAfterAPublishWaitsOutTheThrottlePlusJitter() async {
        let h = Harness()
        let publisher = ProfilePublisher(ports: h.ports())
        await publisher.publishNow(groupIdHex: "g1")   // at 100_000
        h.now = 110_000
        h.jitter = 3_000
        await publisher.noteGrowth(groupIdHex: "g1")?.value
        XCTAssertEqual(h.sleeps, [20_000 + 3_000], "30 s since the last publish, then the jitter")
    }

    func testTheJitterIsBoundedAndTheThrottleClampsABackwardClock() async {
        XCTAssertEqual(ProfilePublisher.growthDelayMs(nowMs: 0, lastPublishedAtMs: nil, jitterMs: 10_000), 10_000)
        XCTAssertEqual(ProfilePublisher.growthDelayMs(nowMs: 500_000, lastPublishedAtMs: 100_000, jitterMs: 0), 0)
        XCTAssertEqual(
            ProfilePublisher.growthDelayMs(nowMs: 0, lastPublishedAtMs: 9_000_000, jitterMs: 0),
            ProfilePublisher.minIntervalMs,
            "a wall clock that stepped back never silences a group for longer than the interval")
        let h = Harness()
        h.jitter = 999_999
        let publisher = ProfilePublisher(ports: h.ports())
        await publisher.noteGrowth(groupIdHex: "g1")?.value
        XCTAssertEqual(h.sleeps, [ProfilePublisher.maxJitterMs], "an out-of-range jitter is clamped")
    }

    func testASecondGrowthCoalesesIntoTheScheduledRepublish() async {
        let h = Harness()
        let publisher = ProfilePublisher(ports: h.ports(holdSleeps: true))
        let first = publisher.noteGrowth(groupIdHex: "g1")
        let second = publisher.noteGrowth(groupIdHex: "g1")
        XCTAssertNotNil(first)
        XCTAssertEqual(first, second, "one scheduled republish per group")
        let other = publisher.noteGrowth(groupIdHex: "g2")
        XCTAssertNotEqual(first, other, "throttling is per group")
        await h.waitForHeldSleeps(2)
        XCTAssertEqual(h.heldSleeps.count, 2, "one wait per group, not one per growth")
        h.releaseSleeps()
        await first?.value
        await other?.value
        XCTAssertEqual(h.sent.map(\.group).sorted(), ["g1", "g2"])
    }

    func testAScheduledRepublishIsSkippedWhenAPublishAlreadyWentOutAfterTheGrowth() async {
        let h = Harness()
        let publisher = ProfilePublisher(ports: h.ports(holdSleeps: true))
        let scheduled = publisher.noteGrowth(groupIdHex: "g1")
        await h.waitForHeldSleeps(1)
        // The user saves a new name while the republish is waiting: it goes out
        // now, sealed at an epoch that already includes the newcomer.
        await publisher.publishNow(groupIdHex: "g1")
        h.releaseSleeps()
        await scheduled?.value
        XCTAssertEqual(h.sent.count, 1, "the growth republish would only repeat what just went out")
    }

    func testAFailedSendIsNotCountedAsPublished() async {
        let h = Harness()
        let publisher = ProfilePublisher(ports: h.ports(holdSleeps: true))
        let scheduled = publisher.noteGrowth(groupIdHex: "g1")
        await h.waitForHeldSleeps(1)
        h.failSends = true
        await publisher.publishNow(groupIdHex: "g1")
        h.failSends = false
        h.releaseSleeps()
        await scheduled?.value
        XCTAssertEqual(h.sent.count, 1, "the failed publish reached nobody, so the republish still fires")
    }
}

/// ADR-0048 — the composition root: received profiles, renames, verification
/// and the own profile meeting in one resolved name.
@MainActor
final class ProfileDirectoryTests: XCTestCase {

    private var suite = ""
    private var defaults: UserDefaults!

    override func setUp() async throws {
        suite = "lakko.tests.profile-directory.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suite)
    }

    private final class Persistence: ProfilePersistence {
        var blobs: [String: Data] = [:]
        func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
        func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
        func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
    }

    private func makeDirectory(
        resolve: @escaping (String) async throws -> UsernameResolution
    ) -> (ProfileDirectory, UsernameVerifier) {
        let verifier = UsernameVerifier(resolve: resolve, defaults: defaults)
        let directory = ProfileDirectory(
            received: ReceivedProfileStore(
                persistence: Persistence(),
                isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
            verifier: verifier,
            ownStore: OwnProfileStore(defaults: defaults)
        )
        directory.accountIdentityProvider = TestProfileSigning.provider
        directory.verifiesOnResolve = false
        directory.setRenames(from: [])
        directory.activate(accountIdB64: "acct", username: "valentin")
        return (directory, verifier)
    }

    // MARK: - Backup received names (2026-11-04)

    /// A backup carries received names: contacts first, then by recent use,
    /// then the rest; never this account's own keys.
    func testABackupCarriesReceivedNamesContactsFirstThenRecentNeverOwn() {
        let contact = base64Encode(Data(repeating: 0x01, count: 32))
        let recent = base64Encode(Data(repeating: 0x02, count: 32))
        let other = base64Encode(Data(repeating: 0x03, count: 32))
        let own = base64Encode(Data(repeating: 0x04, count: 32))
        let profiles: [String: ProfileClaim] = [
            other: .signed(displayName: "Other", updatedAtMs: 1),
            recent: .signed(displayName: "Recent", updatedAtMs: 1),
            contact: .signed(displayName: "Contact", updatedAtMs: 1),
            own: .signed(displayName: "Me", updatedAtMs: 1),
            // ADR-0101 section 8 — a row held without its envelope (a found
            // name) is not carried: nobody re-signs another person's profile.
            base64Encode(Data(repeating: 0x07, count: 32)):
                ProfileClaim(displayName: "Found", username: nil, updatedAtMs: 0),
        ]
        let records = OwnBackupEnvelopes.receivedProfileRecords(
            profiles: profiles, contactKeysB64: [contact], recentPeopleB64: [recent, own],
            ownKeysB64: [own])
        XCTAssertEqual(records.map { base64Encode($0.personKey) }, [contact, recent, other])
        let names = records.compactMap { (try? decodeProfileEnvelope(plaintext: $0.envelope))?.displayName }
        XCTAssertEqual(names, ["Contact", "Recent", "Other"])
    }

    /// Walk 3, W3-R4 — seen on Android 2026-11-20: composing the received
    /// names for a backup (one core call per person) ran on the main thread.
    /// The reads stay on the main actor, the composing runs off it, and what
    /// is backed up is the same as before.
    func testBackupNamesAreComposedOffTheMainThreadAndUnchanged() async {
        let contact = base64Encode(Data(repeating: 0x05, count: 32))
        let other = base64Encode(Data(repeating: 0x06, count: 32))
        let gathered = OwnBackupEnvelopes.Gathered(
            ownProfile: Data([1]), visibility: Data([2]),
            profiles: [
                other: .signed(displayName: "Other", updatedAtMs: 1),
                contact: .signed(displayName: "Contact", updatedAtMs: 1),
            ],
            contactKeysB64: [contact], recentPeopleB64: [], ownKeysB64: [])
        let (assembled, onMain) = await Task.detached {
            (OwnBackupEnvelopes.assemble(gathered), Thread.isMainThread)
        }.value
        XCTAssertFalse(onMain)
        XCTAssertEqual(assembled, OwnBackupEnvelopes(
            ownProfile: Data([1]), visibility: Data([2]),
            receivedProfiles: OwnBackupEnvelopes.receivedProfileRecords(
                profiles: gathered.profiles, contactKeysB64: [contact], recentPeopleB64: [], ownKeysB64: [])))
        XCTAssertEqual(assembled.receivedProfiles.map { base64Encode($0.personKey) }, [contact, other])

        // `current` is the gathered state, assembled.
        let account = "backup-off-main-\(UUID().uuidString)"
        let current = await OwnBackupEnvelopes.current(accountIdB64: account)
        XCTAssertEqual(current, OwnBackupEnvelopes.assemble(OwnBackupEnvelopes.gather(accountIdB64: account)))
    }

    /// Restore feeds each carried name through the receive gate: a key with
    /// nothing held takes it, a held newer stamp is kept, an own key is
    /// skipped.
    func testARestoredBackupNamesPeopleThroughTheReceiveGate() throws {
        let (directory, _) = makeDirectory { _ in .notRegistered }
        let fresh = Data(repeating: 0x11, count: 32)
        let newerHeld = Data(repeating: 0x22, count: 32)
        let own = Data(repeating: 0x33, count: 32)
        directory.acceptReceived(
            ProfileClaim(displayName: "Kept", username: nil, updatedAtMs: 9),
            signatureKeyB64: base64Encode(newerHeld), accountIdB64: directory.accountIdB64)
        func record(_ key: Data, _ name: String, _ stamp: UInt64) throws -> BackupReceivedProfile {
            BackupReceivedProfile(
                personKey: key,
                envelope: try XCTUnwrap(ProfileClaim.signed(displayName: name, updatedAtMs: stamp).envelope))
        }
        let stored = PhraseRestoreBackup.applyReceivedProfiles(
            [try record(fresh, "VG3", 5), try record(newerHeld, "Older", 3), try record(own, "Me", 5)],
            accountIdB64: "acct", context: "test", profiles: directory, ownKeysB64: [base64Encode(own)])
        XCTAssertEqual(stored, 1)
        XCTAssertEqual(directory.resolve(signatureKey: fresh).name, "VG3")
        XCTAssertEqual(directory.resolve(signatureKey: newerHeld).name, "Kept")
        XCTAssertTrue(directory.resolve(signatureKey: own).isPlaceholder)
    }

    func testAReceivedProfileNamesThePersonAndItsUsernameAppearsOnlyAfterVerifying() async {
        let key = Data(repeating: 0x33, count: 32)
        let (directory, verifier) = makeDirectory { _ in .signatureKey(key) }
        let unknown = directory.resolve(signatureKey: key)
        XCTAssertEqual(unknown.name, "Member", "nothing known yet: the neutral placeholder, never the key")
        XCTAssertEqual(unknown.fingerprint, "3333 3333")

        let before = directory.revision
        directory.acceptReceived(
            ProfileClaim(displayName: "Alice Petrova", username: "alice", updatedAtMs: 5),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        XCTAssertGreaterThan(directory.revision, before, "an arriving profile re-renders in place")
        let unverified = directory.resolve(signatureKey: key)
        XCTAssertEqual(unverified.name, "Alice Petrova")
        XCTAssertNil(unverified.verifiedUsername, "being verified: the name without the username")

        await verifier.verify(signatureKeyB64: base64Encode(key), username: "alice")
        XCTAssertEqual(directory.resolve(signatureKey: key).listLabel, "Alice Petrova · @alice")
    }

    // MARK: - The name a person was found under (who-can-find-me section 6.1)

    private func sentRow(
        name: String?, username: String = "", key: Data?
    ) -> SentContactRequest {
        SentContactRequest(
            recipientUsername: username, recipientAccountIdB64: "acct-b", messageId: 1, createdAt: "",
            recipientName: name, recipientKeyB64: key.map { base64Encode($0) })
    }

    /// A directory over `persistence`, verifying as the app does: a check of
    /// the stored username reads no certificate here, so it refuses.
    private func relaunchable(
        _ persistence: Persistence, checks: @escaping () -> Void = {}
    ) -> ProfileDirectory {
        let verifier = UsernameVerifier(resolve: { _ in .notRegistered }, defaults: defaults)
        verifier.verifyBinding = { _, _ in
            checks()
            return false
        }
        let directory = ProfileDirectory(
            received: ReceivedProfileStore(
                persistence: persistence,
                isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
            verifier: verifier,
            ownStore: OwnProfileStore(defaults: defaults))
        directory.accountIdentityProvider = TestProfileSigning.provider
        directory.setRenames(from: [])
        directory.activate(accountIdB64: "acct", username: "valentin")
        return directory
    }

    func testANewContactStandsUnderTheNameTheyWereFoundUnder() async {
        let key = Data(repeating: 0x41, count: 32)
        var checks = 0
        let directory = relaunchable(Persistence()) { checks += 1 }
        let before = directory.revision

        let claim = SentRequests.foundClaim(
            sentRow(name: "Anna Weber", username: "anna_w", key: key), signatureKey: key)
        XCTAssertEqual(
            directory.fileFoundName(
                claim!, signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64),
            .stored)

        XCTAssertGreaterThan(directory.revision, before, "the row re-renders in place")
        let resolved = directory.resolve(signatureKey: key)
        XCTAssertEqual(resolved.name, "Anna Weber")
        XCTAssertFalse(resolved.isPlaceholder)
        XCTAssertEqual(resolved.verifiedUsername, "anna_w", "the search verified it")
        XCTAssertFalse(resolved.usernameNotVerified)
        await Task.yield()
        XCTAssertEqual(checks, 0, "a check of this claim would refuse the pair for good")
    }

    func testTheNameStandsAfterARelaunchLikeAnyProfile() {
        let key = Data(repeating: 0x42, count: 32)
        let persistence = Persistence()
        let first = relaunchable(persistence)
        first.fileFoundName(
            ProfileClaim(displayName: "Anna Weber", username: "anna_w", updatedAtMs: 0),
            signatureKeyB64: base64Encode(key), accountIdB64: first.accountIdB64)

        var checks = 0
        let again = relaunchable(persistence) { checks += 1 }
        let resolved = again.resolve(signatureKey: key)
        XCTAssertEqual(resolved.name, "Anna Weber")
        XCTAssertEqual(resolved.verifiedUsername, "anna_w")
        XCTAssertEqual(checks, 0)
    }

    func testTheirOwnProfileReplacesTheFoundName() {
        let key = Data(repeating: 0x43, count: 32)
        let directory = relaunchable(Persistence())
        directory.fileFoundName(
            ProfileClaim(displayName: "Anna Weber", username: nil, updatedAtMs: 0),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)

        directory.acceptReceived(
            ProfileClaim(displayName: "Anna W", username: nil, updatedAtMs: 7),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        XCTAssertEqual(directory.resolve(signatureKey: key).name, "Anna W")
    }

    /// A person who never set a name publishes at stamp 0, level with the
    /// found name: strictly newer alone would keep the stand-in for good.
    func testAProfileAtStampZeroStillReplacesTheFoundName() {
        let key = Data(repeating: 0x44, count: 32)
        let store = ReceivedProfileStore(
            persistence: Persistence(),
            isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) })
        store.activate(accountIdB64: "acct")
        let keyB64 = base64Encode(key)
        _ = store.accept(
            ProfileClaim(displayName: nil, username: "anna_w", updatedAtMs: 0), signatureKeyB64: keyB64)

        let own = ProfileClaim(
            displayName: nil, username: "anna_w", updatedAtMs: 0,
            accountId: Data(repeating: 1, count: 16), usernameCertificate: Data(repeating: 2, count: 8))
        XCTAssertEqual(store.accept(own, signatureKeyB64: keyB64), .stored)
        XCTAssertEqual(store.profile(signatureKeyB64: keyB64), own)
        XCTAssertFalse(own.isFoundName)
    }

    func testAProfileSayingNothingDoesNotTakeTheNameAway() {
        let key = Data(repeating: 0x45, count: 32)
        let directory = relaunchable(Persistence())
        directory.fileFoundName(
            ProfileClaim(displayName: "Anna Weber", username: nil, updatedAtMs: 0),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        directory.acceptReceived(
            ProfileClaim(
                displayName: nil, username: nil, updatedAtMs: 0, accountId: Data(repeating: 1, count: 16)),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        XCTAssertEqual(directory.resolve(signatureKey: key).name, "Anna Weber")
    }

    func testAFoundNameNeverReplacesAProfile() {
        let key = Data(repeating: 0x46, count: 32)
        let directory = relaunchable(Persistence())
        directory.acceptReceived(
            ProfileClaim(displayName: "Anna W", username: nil, updatedAtMs: 7),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        XCTAssertEqual(
            directory.fileFoundName(
                ProfileClaim(displayName: "Anna Weber", username: nil, updatedAtMs: 0),
                signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64),
            .notNewer)
        XCTAssertEqual(directory.resolve(signatureKey: key).name, "Anna W")
    }

    func testAFoundNameIsFiledOnlyForTheAccountThatFoundIt() {
        let key = Data(repeating: 0x47, count: 32)
        let directory = relaunchable(Persistence())
        XCTAssertEqual(
            directory.fileFoundName(
                ProfileClaim(displayName: "Anna Weber", username: nil, updatedAtMs: 0),
                signatureKeyB64: base64Encode(key), accountIdB64: "another"),
            .inactive)
        XCTAssertTrue(directory.resolve(signatureKey: key).isPlaceholder)
    }

    func testTheFoundClaimIsOnlyEverTheirs() {
        let found = Data(repeating: 0x48, count: 32)
        let other = Data(repeating: 0x49, count: 32)
        let row = sentRow(name: "Anna Weber", key: found)

        XCTAssertEqual(
            SentRequests.foundClaim(row, signatureKey: found),
            ProfileClaim(displayName: "Anna Weber", username: nil, updatedAtMs: 0))
        XCTAssertNil(
            SentRequests.foundClaim(row, signatureKey: other),
            "the row matched by the account id the answer states, and another key signed it")
        XCTAssertNil(SentRequests.foundClaim(sentRow(name: nil, key: found), signatureKey: found))
        XCTAssertNil(SentRequests.foundClaim(sentRow(name: "  ", key: found), signatureKey: found))
        XCTAssertNil(
            SentRequests.foundClaim(sentRow(name: "Anna", key: nil), signatureKey: found),
            "a row that does not say which key it found")
        XCTAssertNil(
            SentRequests.foundClaim(
                sentRow(name: displayFingerprint(found), username: "anna_w", key: found),
                signatureKey: found),
            "a fingerprint names nobody")
    }

    func testAUsernameOnlyNameIsTheUsername() {
        let key = Data(repeating: 0x4A, count: 32)
        XCTAssertEqual(
            SentRequests.foundClaim(sentRow(name: "@anna_w", username: "anna_w", key: key), signatureKey: key),
            ProfileClaim(displayName: nil, username: "anna_w", updatedAtMs: 0))
    }

    /// The profile request asks while someone is unnamed. A found name is not
    /// their profile: counted as a name, the asking that brings it would stop.
    func testAFoundNameStillCountsAsUnnamedForTheProfileRequest() {
        let key = Data(repeating: 0x4E, count: 32)
        let directory = relaunchable(Persistence())
        XCTAssertTrue(directory.countsAsUnnamed(signatureKey: key))

        directory.fileFoundName(
            ProfileClaim(displayName: "Anna Weber", username: "anna_w", updatedAtMs: 0),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        XCTAssertFalse(directory.resolve(signatureKey: key).isPlaceholder)
        XCTAssertTrue(directory.countsAsUnnamed(signatureKey: key))

        directory.acceptReceived(
            ProfileClaim(displayName: "Anna W", username: nil, updatedAtMs: 7),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        XCTAssertFalse(directory.countsAsUnnamed(signatureKey: key))
    }

    /// The verdict stays on this device: a new one reading the username with
    /// no certificate would refuse it for good.
    func testAFoundUsernameDoesNotTravelInAStateBundle() {
        let key = Data(repeating: 0x4B, count: 32)
        let bare = Data(repeating: 0x4C, count: 32)
        let real = Data(repeating: 0x4D, count: 32)
        let directory = relaunchable(Persistence())
        directory.fileFoundName(
            ProfileClaim(displayName: "Anna Weber", username: "anna_w", updatedAtMs: 0),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        directory.fileFoundName(
            ProfileClaim(displayName: nil, username: "anna_w", updatedAtMs: 0),
            signatureKeyB64: base64Encode(bare), accountIdB64: directory.accountIdB64)
        let own = ProfileClaim(
            displayName: "Boris", username: "boris", updatedAtMs: 3,
            usernameCertificate: Data(repeating: 2, count: 8))
        directory.acceptReceived(
            own, signatureKeyB64: base64Encode(real), accountIdB64: directory.accountIdB64)

        let carried = directory.profilesForBundle(accountIdB64: "acct")
        XCTAssertEqual(
            carried[base64Encode(key)],
            ProfileClaim(displayName: "Anna Weber", username: nil, updatedAtMs: 0))
        XCTAssertNil(carried[base64Encode(bare)])
        XCTAssertEqual(carried[base64Encode(real)], own)
    }

    /// A profile received by one account's work is not filed under another
    /// account that became active meanwhile.
    func testAProfileReceivedForAnotherAccountIsNotFiled() {
        let key = Data(repeating: 0x66, count: 32)
        let (directory, _) = makeDirectory { _ in .notRegistered }
        let claim = ProfileClaim(displayName: "Late", username: nil, updatedAtMs: 5)
        XCTAssertEqual(
            directory.acceptReceived(claim, signatureKeyB64: base64Encode(key), accountIdB64: "other"),
            .inactive)
        XCTAssertEqual(
            directory.acceptReceived(claim, signatureKeyB64: base64Encode(key), accountIdB64: nil),
            .inactive)
        XCTAssertNil(directory.storedProfile(signatureKeyB64: base64Encode(key)))
        XCTAssertEqual(
            directory.acceptReceived(claim, signatureKeyB64: base64Encode(key), accountIdB64: "acct"),
            .stored)
        XCTAssertEqual(directory.storedProfile(signatureKeyB64: base64Encode(key))?.displayName, "Late")
    }

    func testAMismatchedUsernameIsNeverShownAndIsFlaggedForDetail() async {
        let key = Data(repeating: 0x33, count: 32)
        let someoneElse = Data(repeating: 0x44, count: 32)
        let (directory, verifier) = makeDirectory { _ in .signatureKey(someoneElse) }
        directory.acceptReceived(
            ProfileClaim(displayName: nil, username: "pavel", updatedAtMs: 5),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        await verifier.verify(signatureKeyB64: base64Encode(key), username: "pavel")
        let resolved = directory.resolve(signatureKey: key)
        XCTAssertEqual(resolved.name, "Member")
        XCTAssertTrue(resolved.isPlaceholder)
        XCTAssertFalse(resolved.listLabel.contains("pavel"))
        XCTAssertTrue(resolved.usernameNotVerified)
    }

    func testALocalRenameWinsAndKeepsTheirNameForDetail() {
        let key = Data(repeating: 0x55, count: 32)
        let (directory, _) = makeDirectory { _ in .notRegistered }
        directory.acceptReceived(
            ProfileClaim(displayName: "Alice Petrova", username: nil, updatedAtMs: 5),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        let contact = AcceptedContact(
            request: IncomingContactRequest(
                messageId: 1, fromPubkey: key, fromAccountId: nil,
                fromPseudonym: Data(count: 32), validAfterMs: 0, intro: nil, createdAt: ""),
            displayName: "Mum")
        directory.setRenames(from: [contact])
        let resolved = directory.resolve(signatureKey: key)
        XCTAssertEqual(resolved.name, "Mum")
        XCTAssertEqual(resolved.theirName, "Alice Petrova")
    }

    // MARK: ADR-0079 - a pending contact is named by its link's statement

    private func pendingContact(
        _ key: Data, name: String, username: String = "", hex: String = "a1"
    ) -> PendingContact {
        PendingContact(
            accountIdHex: String(repeating: hex, count: 16), signaturePubkey: key,
            x25519Pubkey: Data(repeating: 0x01, count: 32), displayName: name,
            username: username, addedAtMs: 1, linkId: Data(repeating: 0x02, count: 16))
    }

    /// Seen live 2026-10-29: a pending contact read "Member" in a group
    /// roster while the contact list named it from the link.
    func testAPendingContactIsNamedByItsLinkLikeAProfileNotARename() {
        let key = Data(repeating: 0x71, count: 32)
        let (directory, _) = makeDirectory { _ in .notRegistered }
        XCTAssertTrue(directory.resolve(signatureKey: key).isPlaceholder)

        let before = directory.revision
        directory.setPendingContacts([pendingContact(key, name: "  VG2 ", username: "vg2")])
        XCTAssertGreaterThan(directory.revision, before, "a pending contact re-renders the names in place")
        let resolved = directory.resolve(signatureKey: key)
        XCTAssertEqual(resolved.name, "VG2")
        XCTAssertEqual(resolved.source, .profileName, "the link states the person's own name, not a rename")
        XCTAssertNil(resolved.verifiedUsername, "the link's username is a claim with no verdict")
        XCTAssertFalse(resolved.usernameNotVerified)
        XCTAssertNil(resolved.theirName)

        let unchanged = directory.revision
        directory.setPendingContacts([pendingContact(key, name: "  VG2 ", username: "vg2")])
        XCTAssertEqual(directory.revision, unchanged, "the same list is not a change")
    }

    func testAReceivedProfileWinsOverAPendingContactsLink() {
        let key = Data(repeating: 0x72, count: 32)
        let (directory, _) = makeDirectory { _ in .notRegistered }
        directory.setPendingContacts([pendingContact(key, name: "Link name")])
        directory.acceptReceived(
            ProfileClaim(displayName: "Current name", username: nil, updatedAtMs: 5),
            signatureKeyB64: base64Encode(key), accountIdB64: directory.accountIdB64)
        XCTAssertEqual(directory.resolve(signatureKey: key).name, "Current name")
    }

    func testALocalRenameWinsOverAPendingContactsLink() {
        let key = Data(repeating: 0x73, count: 32)
        let (directory, _) = makeDirectory { _ in .notRegistered }
        directory.setPendingContacts([pendingContact(key, name: "Link name")])
        let contact = AcceptedContact(
            request: IncomingContactRequest(
                messageId: 1, fromPubkey: key, fromAccountId: nil,
                fromPseudonym: Data(count: 32), validAfterMs: 0, intro: nil, createdAt: ""),
            displayName: "Mum")
        directory.setRenames(from: [contact])
        let resolved = directory.resolve(signatureKey: key)
        XCTAssertEqual(resolved.name, "Mum")
        XCTAssertEqual(resolved.source, .localRename)
    }

    func testRemovingThePendingContactGoesBackToThePlaceholder() {
        let key = Data(repeating: 0x74, count: 32)
        let other = Data(repeating: 0x75, count: 32)
        let (directory, _) = makeDirectory { _ in .notRegistered }
        directory.setPendingContacts([
            pendingContact(key, name: "VG2"), pendingContact(other, name: "Other", hex: "b2"),
        ])
        XCTAssertEqual(directory.resolve(signatureKey: key).name, "VG2")

        let before = directory.revision
        directory.setPendingContacts([pendingContact(other, name: "Other", hex: "b2")])
        XCTAssertGreaterThan(directory.revision, before, "a removal re-renders the names in place")
        XCTAssertEqual(directory.resolve(signatureKey: key).name, "Member")
        XCTAssertEqual(directory.resolve(signatureKey: other).name, "Other")

        directory.setPendingContacts([pendingContact(other, name: "Other", hex: "b2")])
        directory.deactivate()
        XCTAssertTrue(directory.resolve(signatureKey: other).isPlaceholder, "no account, no pending names")
    }

    func testALinkStatingNothingIsNotAName() {
        let key = Data(repeating: 0x76, count: 32)
        let (directory, _) = makeDirectory { _ in .notRegistered }
        let before = directory.revision
        directory.setPendingContacts([pendingContact(key, name: "   ")])
        XCTAssertEqual(directory.revision, before)
        XCTAssertTrue(directory.resolve(signatureKey: key).isPlaceholder)
    }

    /// The link's username is shown only with a verdict for the pair, and a
    /// resolve never starts a check for it: the check reads the certificate
    /// a received profile carried, and a link carries none, so a check would
    /// record a refusal a later profile with a certificate could not undo.
    func testAPendingContactsUsernameIsShownOnlyWithAVerdictAndNeverStartsACheck() async {
        let key = Data(repeating: 0x77, count: 32)
        final class Counter { var lookups = 0 }
        let counter = Counter()
        let (directory, verifier) = makeDirectory { _ in
            counter.lookups += 1
            return .signatureKey(key)
        }
        directory.verifiesOnResolve = true
        directory.setPendingContacts([pendingContact(key, name: "", username: "vg2")])
        let unverified = directory.resolve(signatureKey: key)
        XCTAssertTrue(unverified.isPlaceholder, "a username without a verdict is no name")
        for _ in 0..<5 { await Task.yield() }
        XCTAssertEqual(counter.lookups, 0, "no check is started for a link's username")
        XCTAssertNil(verifier.state(signatureKeyB64: base64Encode(key), username: "vg2"))

        await verifier.verify(signatureKeyB64: base64Encode(key), username: "vg2")
        let verified = directory.resolve(signatureKey: key)
        XCTAssertEqual(verified.name, "@vg2")
        XCTAssertEqual(verified.source, .verifiedUsername)
    }

    /// The "is something known about this key" predicate counts a pending
    /// contact: a key known only by its link is not resolved as a proven
    /// predecessor's name, and a key known by nothing resolves through a
    /// predecessor known only by its link.
    func testAPendingContactCountsAsSomethingKnownAcrossAProvenRotation() {
        let oldKey = Data(repeating: 0x78, count: 32)
        let newKey = Data(repeating: 0x79, count: 32)
        let (directory, _) = makeDirectory { _ in .notRegistered }
        var map = ProvenPredecessors()
        map.record(oldKeyB64: base64Encode(oldKey), newKeyB64: base64Encode(newKey), evidence: .verifiedProof)
        directory.setProvenPredecessors(map)

        directory.setPendingContacts([pendingContact(oldKey, name: "Linked")])
        XCTAssertEqual(directory.resolve(signatureKey: newKey).name, "Linked", "the same person, by proof")

        directory.acceptReceived(
            ProfileClaim(displayName: "Old profile", username: nil, updatedAtMs: 5),
            signatureKeyB64: base64Encode(oldKey), accountIdB64: directory.accountIdB64)
        directory.setPendingContacts([pendingContact(newKey, name: "Linked now")])
        XCTAssertEqual(
            directory.resolve(signatureKey: newKey).name, "Linked now",
            "a key known by its own link is not resolved through another key")
    }

    func testTheOwnProfileEnvelopeRoundTripsThroughTheCore() throws {
        let (directory, _) = makeDirectory { _ in .notRegistered }
        try directory.setOwnDisplayName("  Valentin  ")
        let envelope = try XCTUnwrap(directory.ownProfileEnvelope())
        let decoded = try decodeProfileEnvelope(plaintext: envelope)
        XCTAssertEqual(decoded.displayName, "Valentin")
        // ADR-0060 section 5 — a username travels only with its certificate;
        // this directory holds none, so the envelope names no username (a v3
        // receiver would never show one it cannot verify).
        XCTAssertNil(decoded.username)
        XCTAssertEqual(decoded.updatedAtMs, directory.ownProfile?.updatedAtMs)

        directory.activate(accountIdB64: "guest", username: "")
        try directory.setOwnDisplayName("Guest")
        let guest = try decodeProfileEnvelope(plaintext: XCTUnwrap(directory.ownProfileEnvelope()))
        XCTAssertEqual(guest.displayName, "Guest")
        XCTAssertNil(guest.username, "a guest publishes a name and no username")

        directory.deactivate()
        XCTAssertNil(directory.ownProfileEnvelope(), "no account, nothing to publish")
        XCTAssertNil(directory.ownProfileForRequest())
    }


    // MARK: - The profile key along an alias (ADR-0073 R.2, UX 16.3)

    private func stamps(_ held: [String: UInt64]) -> (String) -> UInt64? { { held[$0] } }

    func testAKeyWithItsOwnProfileIsItsOwnAnswer() {
        XCTAssertEqual(
            ProfileDirectory.profileKey(for: "new", aliases: ["old": "new"], stampOf: stamps(["new": 2, "old": 1])),
            "new")
    }

    /// Seen live 2026-11-25: the successor's roster named the new key, which
    /// had sent no profile yet, and the row read "Member".
    func testTheNewKeyOfAResetContactFallsBackToTheProfileTheirOldKeySent() {
        XCTAssertEqual(
            ProfileDirectory.profileKey(for: "new", aliases: ["old": "new"], stampOf: stamps(["old": 1])), "old")
    }

    func testOfSeveralRetiredKeysTheNewestProfileNamesThePerson() {
        let aliases = ["first": "new", "second": "new"]
        XCTAssertEqual(
            ProfileDirectory.profileKey(for: "new", aliases: aliases, stampOf: stamps(["first": 5, "second": 9])),
            "second")
        XCTAssertEqual(
            ProfileDirectory.profileKey(for: "new", aliases: aliases, stampOf: stamps(["first": 9, "second": 5])),
            "first")
    }

    func testARetiredKeyFallsBackToTheCurrentKeyThenToItsSiblingRetiredKeys() {
        let aliases = ["first": "new", "second": "new"]
        XCTAssertEqual(
            ProfileDirectory.profileKey(for: "first", aliases: aliases, stampOf: stamps(["new": 1, "second": 9])),
            "new")
        XCTAssertEqual(
            ProfileDirectory.profileKey(for: "first", aliases: aliases, stampOf: stamps(["second": 9])), "second")
    }

    func testNeverBorrowsAnotherPersonsProfile() {
        let aliases = ["old": "new", "bobOld": "bob"]
        XCTAssertNil(ProfileDirectory.profileKey(for: "new", aliases: aliases, stampOf: stamps(["bobOld": 3, "bob": 4])))
        XCTAssertNil(ProfileDirectory.profileKey(for: "stranger", aliases: aliases, stampOf: stamps(["old": 1])))
    }

    /// Through the directory: the reset contact's NEW key reads the name and
    /// photo their old key's profile carried, until the new key sends its own.
    func testTheDirectoryNamesTheNewKeyFromTheRetiredKeysProfile() throws {
        final class Persistence: ProfilePersistence {
            var blobs: [String: Data] = [:]
            func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
            func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
            func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
        }
        let suite = "profile-key-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let directory = ProfileDirectory(
            received: ReceivedProfileStore(
                persistence: Persistence(),
                isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
            verifier: UsernameVerifier(resolve: { _ in .notRegistered }, defaults: defaults),
            ownStore: OwnProfileStore(defaults: defaults))
        directory.accountIdentityProvider = TestProfileSigning.provider
        directory.verifiesOnResolve = false
        directory.activate(accountIdB64: suite, username: nil)
        let oldKey = Data(repeating: 0xA1, count: 32)
        let newKey = Data(repeating: 0xB2, count: 32)
        let stranger = Data(repeating: 0xC3, count: 32)
        directory.acceptReceived(
            ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 5),
            signatureKeyB64: base64Encode(oldKey), accountIdB64: suite)
        let placeholder = directory.resolve(signatureKey: newKey)
        XCTAssertTrue(placeholder.isPlaceholder, "no alias yet: the new key is nobody known")

        directory.setRenames(from: [AcceptedContact(
            request: IncomingContactRequest(
                messageId: 1, fromPubkey: newKey, fromAccountId: nil, fromPseudonym: Data(repeating: 1, count: 32),
                validAfterMs: 0, intro: nil, createdAt: "2026-11-25T00:00:00Z"),
            previousKeysB64: [base64Encode(oldKey)])])
        XCTAssertEqual(directory.resolve(signatureKey: newKey).name, "Anna")
        XCTAssertEqual(directory.resolve(signatureKey: oldKey).name, "Anna")
        XCTAssertTrue(directory.resolve(signatureKey: stranger).isPlaceholder)

        // The new identity's own profile, once sent, wins for its key.
        directory.acceptReceived(
            ProfileClaim(displayName: "Anna N", username: nil, updatedAtMs: 7),
            signatureKeyB64: base64Encode(newKey), accountIdB64: suite)
        XCTAssertEqual(directory.resolve(signatureKey: newKey).name, "Anna N")
    }
}
