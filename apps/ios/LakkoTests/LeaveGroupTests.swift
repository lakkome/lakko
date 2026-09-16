// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0045 — `Leave group` on iOS: the decision rule that gates the control,
/// and the live-MLS behaviour of the action behind it.
///
/// ## Two halves, and why the split is where it is
///
/// The **rule** (`leaveGroupAffordance`) is pure, so it is driven directly.
/// The **action** needs a real group — the property that matters is that a
/// member's leave produces a *proposal* rather than a commit, and that the
/// owner's does not reach the wire at all — so it runs against a real
/// `ChatRepository` over the scripted DS, which the simulator target can do
/// because it links the FFI. The Android twin can only cover the rule (a JVM
/// unit test cannot load the JNI library), so the live half is pinned once,
/// here, exactly as `ExternalJoinApprovalTests` pins the approve half.
///
/// ## Discrimination (measured by RUNNING each break, quoted verbatim — not
/// predicted)
///
/// **Break 1 — `leaveGroupAffordance` stubbed to `return .enabled`** (its
/// pre-implementation shape). 5 of 12 fail, e.g.
///   - `testTheOwnerIsNotOfferedTheControl`: `XCTAssertEqual failed: ("enabled")
///     is not equal to ("hidden(Lakko.LeaveGroupRefusal.ownerCannotLeave)") -
///     the owner cannot leave while owner: set_group_roles is owner-only, so an
///     ownerless group could never appoint another admin`
///   - alongside `testALegacyGroupIsNotOfferedTheControl`,
///     `testAnUnresolvableOwnLeafIsNotOfferedTheControl`,
///     `testAnAlreadyRequestedLeaveDisablesTheControl`,
///     `testABusyPanelDisablesTheControl`.
///
/// **Break 2 — the owner arm alone deleted** (`if false { … }`). Exactly 1 fails:
///   - `testTheOwnerIsNotOfferedTheControl`: `XCTAssertEqual failed: ("enabled")
///     is not equal to ("hidden(Lakko.LeaveGroupRefusal.ownerCannotLeave)")`.
///
///   Note what does NOT move: `testTheOwnerIsRefusedByTheCoreAndNothingReachesTheWire`
///   still passes, because it drives the repository directly and the refusal it
///   pins is the CORE's, not this gate's. The two are deliberately independent —
///   the gate exists so the user never meets the core's refusal, not to be the
///   refusal — and the pair is what shows the defence is layered rather than
///   duplicated.
///
/// **Break 3 — the disabled check keyed on the first pending request instead of
/// our own leaf** (`leafIndex: requests.first?.targetLeaf ?? ownLeaf`). Exactly
/// 1 fails:
///   - `testAnotherMembersPendingRemovalDoesNotDisableOurLeave`:
///     `XCTAssertEqual failed: ("disabled") is not equal to ("enabled") - a
///     removal aimed at somebody else says nothing about whether we may ask to
///     leave`.
///
/// **Break 4 — a self-leaf guard added to `ChatRepository.removeMemberFromGroup`**
/// (the shape the view model's `onRemoveMember` still carries, leaking one layer
/// down). 3 of the 4 live-MLS tests fail:
///   - `testAMembersLeaveProducesAProposalAndPublishesIt`,
///     `testAPendingOwnLeaveIsVisibleToTheMemberWhoMadeIt` and
///     `testTheLeavingMemberStaysInTheRosterAnnotatedRatherThanDropped` all fail
///     with `a member's leave must be accepted — naming your OWN leaf needs no
///     role, which is what leaving IS — but it threw: argument("you cannot
///     remove yourself from the group")`.
///
///   `testTheOwnerIsRefusedByTheCoreAndNothingReachesTheWire` correctly stays
///   green there: it asserts a refusal, and a refusal is what it got.
@MainActor
final class LeaveGroupTests: XCTestCase {

    override func setUp() {
        super.setUp()
        // Abort on the first failure rather than recording and continuing: the
        // live-MLS half builds state step by step, so a later assertion running
        // against a fixture that already failed reports a second, derived
        // failure that hides the first.
        continueAfterFailure = false
    }

    // MARK: - Fixtures for the pure rule

    /// A pending request as the fold produces one. Constructed directly rather
    /// than folded, so each test states only the field it is about.
    private func request(
        key: String,
        kind: PendingRequestKind,
        targetLeaf: UInt32?,
        senderLeaf: UInt32?,
        own: Bool,
        state: PendingRequestState = .waiting
    ) -> PendingRequest {
        PendingRequest(
            key: key,
            kind: kind,
            addSignatureKeyB64: nil,
            targetLeaf: targetLeaf,
            senderLeaf: senderLeaf,
            observedAtMs: 0,
            own: own,
            state: state)
    }

    /// Our own leave, already published and past the quiet window.
    private func ownLeaveRequest(leaf: UInt32) -> PendingRequest {
        request(
            key: "remove:\(leaf)", kind: .remove, targetLeaf: leaf, senderLeaf: leaf, own: true)
    }

    // MARK: - (1) The rule: who is offered the control

    /// The ordinary case, and the whole point of the feature: a plain member of
    /// a role-tabled group may ask to leave. The core carries the matching
    /// exception — "Removing exactly your own leaf is leaving, and needs no
    /// role" — so no role gate applies here.
    func testAPlainMemberOnTheProposalPathIsOfferedTheControl() {
        XCTAssertEqual(
            leaveGroupAffordance(
                usesProposalPath: true, isOwner: false, ownLeaf: 3, busy: false, requests: []),
            .enabled,
            "a plain member naming their own leaf needs no role — that is what leaving IS")
    }

    /// The owner cannot leave *while owner*: `set_group_roles` is owner-only, so
    /// an ownerless group could never appoint another admin again. The core
    /// refuses it fail-closed; the UI must not offer a control that always
    /// refuses (UX §6), so this is `hidden`, not `disabled`.
    func testTheOwnerIsNotOfferedTheControl() {
        XCTAssertEqual(
            leaveGroupAffordance(
                usesProposalPath: true, isOwner: true, ownLeaf: 0, busy: false, requests: []),
            .hidden(.ownerCannotLeave),
            "the owner cannot leave while owner: set_group_roles is owner-only, so an "
                + "ownerless group could never appoint another admin")
        XCTAssertEqual(
            LeaveGroupRefusal.ownerCannotLeave.message,
            "The group owner cannot leave the group.",
            "the copy is the UX §6 refusal table verbatim")
    }

    /// A group with no role table has no committer, so nothing would ever sweep
    /// the proposal — and the core's self-remove route does not exist there
    /// either. Offering the control would produce a request that can never
    /// apply.
    func testALegacyGroupIsNotOfferedTheControl() {
        XCTAssertEqual(
            leaveGroupAffordance(
                usesProposalPath: false, isOwner: false, ownLeaf: 3, busy: false, requests: []),
            .hidden(.notRequestable),
            "no role table means no committer, so a leave proposal would never be swept")
    }

    /// Fail-closed: a leave names a leaf, so an unresolvable own leaf is not a
    /// leave we can compose. The fallback copy must never leak a reason.
    func testAnUnresolvableOwnLeafIsNotOfferedTheControl() {
        XCTAssertEqual(
            leaveGroupAffordance(
                usesProposalPath: true, isOwner: false, ownLeaf: nil, busy: false, requests: []),
            .hidden(.notRequestable),
            "a leave names a leaf; without one there is nothing to propose")
        XCTAssertEqual(
            LeaveGroupRefusal.notRequestable.message,
            "This change could not be requested.",
            "the UX §6 fail-closed fallback, which must never leak an internal reason")
    }

    // MARK: - (2) The rule: when the control is live

    /// UX §5.3 — an already-published request keeps its control VISIBLE but
    /// disabled, exactly as a pending removal does. Hiding it would read as the
    /// permission having vanished, where the row's own `Waiting to leave.`
    /// annotation explains a disabled control. There is deliberately no
    /// `Cancel`: a published request cannot be withdrawn.
    func testAnAlreadyRequestedLeaveDisablesTheControl() {
        XCTAssertEqual(
            leaveGroupAffordance(
                usesProposalPath: true, isOwner: false, ownLeaf: 3, busy: false,
                requests: [ownLeaveRequest(leaf: 3)]),
            .disabled,
            "asking twice would put two identical Remove proposals in one sweep")
    }

    /// Another membership action is already in flight.
    func testABusyPanelDisablesTheControl() {
        XCTAssertEqual(
            leaveGroupAffordance(
                usesProposalPath: true, isOwner: false, ownLeaf: 3, busy: true, requests: []),
            .disabled)
    }

    /// The disabled check must be keyed on OUR leaf. A removal aimed at somebody
    /// else is the ordinary state of a group between any admin's request and the
    /// next sweep, and it says nothing about whether we may ask to leave.
    func testAnotherMembersPendingRemovalDoesNotDisableOurLeave() {
        let someoneElse = request(
            key: "remove:7", kind: .remove, targetLeaf: 7, senderLeaf: 0, own: false)
        XCTAssertEqual(
            leaveGroupAffordance(
                usesProposalPath: true, isOwner: false, ownLeaf: 3, busy: false,
                requests: [someoneElse]),
            .enabled,
            "a removal aimed at somebody else says nothing about whether we may ask to leave")
    }

    /// A request still inside the quiet window has not rendered anything yet, so
    /// it must not silently disable the control either — the user would see a
    /// dead button with no explanation beside it.
    func testARequestInsideTheQuietWindowDoesNotDisableTheControl() {
        let notYetVisible = request(
            key: "remove:3", kind: .remove, targetLeaf: 3, senderLeaf: 3, own: true,
            state: .pending)
        XCTAssertEqual(
            leaveGroupAffordance(
                usesProposalPath: true, isOwner: false, ownLeaf: 3, busy: false,
                requests: [notYetVisible]),
            .enabled,
            "nothing is rendered inside the quiet window, so nothing may explain a "
                + "disabled control there")
    }

    // MARK: - (3) The action, against a real MLS group

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        /// The device under test's own leaf.
        let ownLeaf: UInt32
        /// A third member, never touched — proves the roster is not truncated.
        let thirdLeaf: UInt32
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let outboxDir: URL
        /// This fixture's private journal on the scripted DS — see the scoping
        /// note on `ScriptedDsURLProtocol`. `asOwner: true` reaches the ADR-0042
        /// mint on bootstrap, which PUBLISHES from a fire-and-forget `Task`, so
        /// this fixture is one of the stragglers the scoping contains.
        let dsScope: String
    }

    /// Owner A + plain member B + plain member C, with B (or A, per `asOwner`)
    /// rehydrated into a real repository over the scripted DS.
    private func makeRepo(asOwner: Bool) async throws -> Fixture {
        let dsScope = ScriptedDsURLProtocol.freshScope()

        let ownerSeed = Data(repeating: 0x11, count: 32)
        let memberSeed = Data(repeating: 0x55, count: 32)
        let owner = try Identity.fromSeed(seed: ownerSeed)
        let member = try Identity.fromSeed(seed: memberSeed)
        let third = try Identity.fromSeed(seed: Data(repeating: 0x77, count: 32))

        // `Group.create` seeds the ADR-0022 role table, which is what routes
        // this group through ADR-0045's proposal path at all.
        let ownerGroup = try Group.create(identity: owner)
        let addMember = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try member.createKeyPackage())
        let memberGroup = try Group.joinFromWelcome(
            welcome: addMember.welcome, identity: member)
        let addThird = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try third.createKeyPackage())
        _ = try memberGroup.processIncomingVerifyingRoles(identity: member, bytes: addThird.commit)

        let underTest = asOwner ? owner : member
        let underTestGroup = asOwner ? ownerGroup : memberGroup
        let underTestSeed = asOwner ? ownerSeed : memberSeed

        XCTAssertEqual(
            try underTestGroup.epoch(), try ownerGroup.epoch(),
            "the fixture must leave the device under test at the owner's epoch, or its "
                + "proposal would be dropped as wrong-epoch and the suite would pass vacuously")

        let groupId = try underTestGroup.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        let members = try underTestGroup.members()
        let ownKey = try underTest.signaturePublicKey()
        let thirdKey = try third.signaturePublicKey()
        let ownLeaf = try XCTUnwrap(
            members.first { $0.signaturePublicKey == ownKey }?.leafIndex)
        let thirdLeaf = try XCTUnwrap(
            members.first { $0.signaturePublicKey == thirdKey }?.leafIndex)

        let snapshotBlob = try underTest.snapshotStorage()
        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        let suffix = UUID().uuidString
        let knownGroupsStore = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "leave-k-\(suffix)")!)
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("leave-\(suffix)")
        // Created eagerly so the teardown's removal is never itself an error —
        // an incidental teardown failure reported as the test's own outcome
        // masks whatever actually went wrong.
        try FileManager.default.createDirectory(
            at: outboxDir, withIntermediateDirectories: true)

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [ScriptedDsURLProtocol.self]
        // ONE scripted DS behind both seams: the repository publishes through
        // its own `dsClient` but polls through `ChatClient`'s.
        let ds = DsClient(
            baseURL: ScriptedDsURLProtocol.baseURL(dsScope),
            session: URLSession(configuration: config))
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: PushGroupRouting(),
            beaconStore: BeaconObservationStore(
                defaults: UserDefaults(suiteName: "leave-b-\(suffix)")!),
            pendingRequestStore: PendingRequestStore(
                defaults: UserDefaults(suiteName: "leave-p-\(suffix)")!)
        )
        // Phase 1 fixture: the chats run on the account identity the snapshot
        // was taken under (ADR-0050 device identities are exercised elsewhere).
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: underTestSeed,
                sessionToken: "t",
                sessionExpiresAtMs: 0
            )
        )
        XCTAssertTrue(
            repo.usesProposalPath(groupIdHex: groupIdHex),
            "the group must route membership through ADR-0045 proposals, or none of this applies")
        XCTAssertEqual(
            repo.ownRole(groupIdHex: groupIdHex), asOwner ? .owner : .member,
            "the device under test must hold the role this case is about")

        return Fixture(
            repo: repo,
            groupIdHex: groupIdHex,
            ownLeaf: ownLeaf,
            thirdLeaf: thirdLeaf,
            accountIdB64: accountIdB64,
            snapshotStore: snapshotStore,
            outboxDir: outboxDir,
            dsScope: dsScope)
    }

    /// Ask to leave, reporting any refusal as this test's own failure with the
    /// underlying error attached. Letting the error propagate instead makes
    /// XCTest report whatever surfaces during unwinding, which names the wrong
    /// thing and hides the refusal that actually happened.
    @discardableResult
    private func leave(
        _ fx: Fixture, file: StaticString = #filePath, line: UInt = #line
    ) async throws -> MembershipOutcome {
        do {
            return try await fx.repo.removeMemberFromGroup(
                groupIdHex: fx.groupIdHex, leafIndex: fx.ownLeaf)
        } catch {
            XCTFail(
                "a member's leave must be accepted — naming your OWN leaf needs no role, "
                    + "which is what leaving IS — but it threw: \(error)",
                file: file, line: line)
            throw error
        }
    }

    private func tearDown(_ fx: Fixture) {
        fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
        try? FileManager.default.removeItem(at: fx.outboxDir)
        ScriptedDsURLProtocol.reset()
    }

    /// THE CASE THE FEATURE EXISTS FOR. A plain member's leave produces a
    /// PROPOSAL — not a commit — and publishes it to the group journal. The
    /// epoch must not move: a proposal advances nothing, which is the whole
    /// reason concurrent intents compose instead of forking.
    func testAMembersLeaveProducesAProposalAndPublishesIt() async throws {
        let fx = try await makeRepo(asOwner: false)
        defer { tearDown(fx) }

        let epochBefore = try XCTUnwrap(fx.repo.epoch(groupIdHex: fx.groupIdHex))
        let rowsBefore = ScriptedDsURLProtocol.rowCount(scope: fx.dsScope)

        let outcome = try await leave(fx)

        guard case .proposed = outcome else {
            XCTFail(
                "a leave in a role-tabled group must be a PROPOSAL — a member cannot commit "
                    + "their own removal (OpenMLS CannotRemoveSelf) — but got \(outcome)")
            return
        }
        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: fx.dsScope), rowsBefore + 1,
            "the proposal must reach the journal, or no committer can ever sweep it")
        XCTAssertEqual(
            try fx.repo.epoch(groupIdHex: fx.groupIdHex), epochBefore,
            "a proposal advances no epoch — that is what lets concurrent intents compose")
    }

    /// The owner's leave must be refused, and refused BEFORE anything is
    /// published. The core's `authorise` refuses it because the target leaf is
    /// the owner's; the point pinned here is that nothing reaches the wire, so
    /// no peer ever holds a request that can never be swept.
    func testTheOwnerIsRefusedByTheCoreAndNothingReachesTheWire() async throws {
        let fx = try await makeRepo(asOwner: true)
        defer { tearDown(fx) }

        let rowsBefore = ScriptedDsURLProtocol.rowCount(scope: fx.dsScope)
        do {
            _ = try await fx.repo.removeMemberFromGroup(
                groupIdHex: fx.groupIdHex, leafIndex: fx.ownLeaf)
            XCTFail(
                "the owner's leave must be refused — set_group_roles is owner-only, so an "
                    + "ownerless group could never appoint another admin again")
        } catch {
            // Expected. The message is the core's own and is deliberately NOT
            // matched here: mapping core prose to fixed copy is the drift class
            // UX §12.6 flags, which is why the UI hides the control instead.
        }
        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: fx.dsScope), rowsBefore,
            "the owner's leave must never reach the DS, but the journal grew from "
                + "\(rowsBefore) to \(ScriptedDsURLProtocol.rowCount(scope: fx.dsScope)) rows")
    }

    /// UX §10's non-negotiable exception. A plain member sees no pending request
    /// at all — the set is a work queue only owner/admins have a verb over —
    /// EXCEPT one they made themselves. Without this, `Leave group` is a button
    /// with no feedback.
    func testAPendingOwnLeaveIsVisibleToTheMemberWhoMadeIt() async throws {
        let fx = try await makeRepo(asOwner: false)
        defer { tearDown(fx) }

        _ = try await leave(fx)

        XCTAssertEqual(
            fx.repo.ownRole(groupIdHex: fx.groupIdHex), .member,
            "the visibility exception only means something for a device with no admin rights")
        let visible = visibleRequests(
            fx.repo.pendingRequests(groupIdHex: fx.groupIdHex), canManageMembers: false)
        XCTAssertEqual(
            visible.count, 1,
            "a member must see the status of a request they made themselves, or Leave group "
                + "is a button with no feedback")
        XCTAssertEqual(visible.first?.own, true)
        XCTAssertEqual(visible.first?.targetLeaf, fx.ownLeaf)
    }

    /// UX §3(a) — NO optimistic update. Until a committer sweeps, the leaving
    /// member is still cryptographically in the group: still receiving every
    /// message, still able to decrypt. The roster must keep the row and annotate
    /// it, because dropping it would assert a departure that has not happened —
    /// the membership equivalent of a green E2EE pill over an unencrypted call.
    func testTheLeavingMemberStaysInTheRosterAnnotatedRatherThanDropped() async throws {
        let fx = try await makeRepo(asOwner: false)
        defer { tearDown(fx) }

        let before = try XCTUnwrap(fx.repo.members(groupIdHex: fx.groupIdHex)).count
        _ = try await leave(fx)

        let after = try XCTUnwrap(fx.repo.members(groupIdHex: fx.groupIdHex))
        XCTAssertEqual(
            after.count, before,
            "the leaving member is still in the group until a sweep lands; dropping the row "
                + "would assert a departure that has not happened")
        XCTAssertTrue(
            after.contains { $0.leafIndex == fx.ownLeaf },
            "our own row must still be there — it is the row the annotation hangs on")
        XCTAssertTrue(
            after.contains { $0.leafIndex == fx.thirdLeaf },
            "and nobody else's row may be disturbed by our request")

        let visible = visibleRequests(
            fx.repo.pendingRequests(groupIdHex: fx.groupIdHex), canManageMembers: false)
        XCTAssertEqual(
            rowAnnotation(visible, leafIndex: fx.ownLeaf, ownLeaf: fx.ownLeaf),
            "Waiting to leave.",
            "our own row reads `Waiting to leave.`, never `Waiting to be removed.`")
        XCTAssertNil(
            rowAnnotation(visible, leafIndex: fx.thirdLeaf, ownLeaf: fx.ownLeaf),
            "and no other member's row is annotated by our leaving")
    }
}
