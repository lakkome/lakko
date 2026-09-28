// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

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
            XCTAssertEqual(($0 as? DisplayNameRuleViolation)?.message, "Your name must fit on one line.")
        }
        XCTAssertThrowsError(try checkDisplayName("Alice\u{202E}")) {
            XCTAssertEqual(
                ($0 as? DisplayNameRuleViolation)?.message,
                "Your name contains an invisible or text-direction character.")
        }
        XCTAssertThrowsError(try checkDisplayName(String(repeating: "a", count: 65))) {
            XCTAssertEqual(($0 as? DisplayNameRuleViolation)?.message, "At most 64 characters.")
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
        let verifier = UsernameVerifier(resolve: resolve, nowMs: now, retryAfterMs: 60_000, defaults: defaults)
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

    func testATransientFailureIsRetriedOnlyAfterItsWindow() async {
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
        now = 30_000
        fail = false
        let stillFailed = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        XCTAssertEqual(stillFailed, .failed(atMs: 1_000))
        XCTAssertEqual(log.calls.count, 1)
        now = 61_000
        XCTAssertTrue(verifier.needsVerification(signatureKeyB64: keyAB64, username: "alice"))
        let retried = await verifier.verify(signatureKeyB64: keyAB64, username: "alice")
        XCTAssertEqual(retried, .verified)
        XCTAssertEqual(log.calls.count, 2)
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
                displayName: "Alice Petrova", username: "alice", updatedAtMs: 42, accountId: accountId))
        let event = classify(bytes)
        guard case .profile(let messageId, let profile, let senderKey, _) = event else {
            return XCTFail("a profile envelope must classify as .profile, got \(event)")
        }
        XCTAssertEqual(messageId, 9)
        XCTAssertEqual(
            profile,
            ProfileClaim(
                displayName: "Alice Petrova", username: "alice", updatedAtMs: 42, accountId: accountId))
        XCTAssertEqual(senderKey, "member-3", "bound to the identity the sender leaf names at receipt")
        guard case .profile(_, _, .none, _) = classify(bytes, senderLeaf: nil) else {
            return XCTFail("an unattributable profile still never becomes a bubble")
        }
    }

    func testWithoutTheArmTheProfileWouldRenderAsText() throws {
        let bytes = try composeProfileEnvelope(
            profile: FfiProfile(displayName: "Alice", username: nil, updatedAtMs: 1, accountId: nil))
        let parsed = parseBodyEnvelope(bytes: bytes)
        XCTAssertEqual(parsed.kind, .chat, "the legacy fallback degrades it to a chat body")
        XCTAssertTrue(parsed.text.contains("lakko/profile/v1"))
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
                envelope: { [unowned self] in self.envelope },
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
        directory.verifiesOnResolve = false
        directory.setRenames(from: [])
        directory.activate(accountIdB64: "acct", username: "valentin")
        return (directory, verifier)
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
            signatureKeyB64: base64Encode(key))
        XCTAssertGreaterThan(directory.revision, before, "an arriving profile re-renders in place")
        let unverified = directory.resolve(signatureKey: key)
        XCTAssertEqual(unverified.name, "Alice Petrova")
        XCTAssertNil(unverified.verifiedUsername, "being verified: the name without the username")

        await verifier.verify(signatureKeyB64: base64Encode(key), username: "alice")
        XCTAssertEqual(directory.resolve(signatureKey: key).listLabel, "Alice Petrova · @alice")
    }

    func testAMismatchedUsernameIsNeverShownAndIsFlaggedForDetail() async {
        let key = Data(repeating: 0x33, count: 32)
        let someoneElse = Data(repeating: 0x44, count: 32)
        let (directory, verifier) = makeDirectory { _ in .signatureKey(someoneElse) }
        directory.acceptReceived(
            ProfileClaim(displayName: nil, username: "pavel", updatedAtMs: 5),
            signatureKeyB64: base64Encode(key))
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
            signatureKeyB64: base64Encode(key))
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

    func testTheOwnProfileEnvelopeRoundTripsThroughTheCore() throws {
        let (directory, _) = makeDirectory { _ in .notRegistered }
        try directory.setOwnDisplayName("  Valentin  ")
        let envelope = try XCTUnwrap(directory.ownProfileEnvelope())
        let decoded = try decodeProfileEnvelope(plaintext: envelope)
        XCTAssertEqual(decoded.displayName, "Valentin")
        XCTAssertEqual(decoded.username, "valentin")
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

    func testOwnProfilePreviewShapes() {
        XCTAssertEqual(ownProfilePreview(displayName: "Alice Petrova", username: "alice"), "Alice Petrova · @alice")
        XCTAssertEqual(ownProfilePreview(displayName: "Guest", username: nil), "Guest")
        XCTAssertEqual(ownProfilePreview(displayName: nil, username: "alice"), "@alice")
        XCTAssertNil(ownProfilePreview(displayName: "  ", username: ""))
    }
}
