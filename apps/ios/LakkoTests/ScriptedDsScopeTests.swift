// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// The scripted DS's per-fixture journal scoping, and the hazard it exists for.
///
/// ## The hazard, stated from what the code does rather than from a symptom
///
/// `ScriptedDsURLProtocol` is a `URLProtocol` subclass, so every piece of state
/// it keeps is necessarily `static` and therefore shared by the whole test
/// bundle. Against that, `ChatRepository.bootstrap` calls `ensurePushRouting`
/// for each restored group, which is `Task { @MainActor }` **fire and forget**;
/// on a fixture whose device is the group OWNER that reaches the ADR-0042 mint
/// branch, composes a push-routing-secret envelope, and **publishes it to the
/// group journal** through the very `DsClient` this stub stands in for.
///
/// `OwnershipTransferTests.settledRowCount()` records that publish as MEASURED,
/// landing roughly 200 ms after the call that started it, and works around it
/// *within* its own class. What no per-class workaround can reach is the
/// cross-class case: in a SYNCHRONOUS test the main actor never yields inside
/// the test body, so the `Task` cannot run until the test RETURNS — i.e. during
/// a later test, which may be an `async` one asserting an exact `rowCount`. A
/// shared `reset()` is no defence: the reset runs first and the row lands after.
///
/// ## Why the fix is scoping rather than a tolerance
///
/// Widening an assertion to a delta, or waiting for quiet, makes the test pass
/// without making the bleed impossible — and a test that is *usually* right
/// teaches its readers to re-run rather than read. Keying the journal on the
/// request host, with a unique host per fixture, makes a straggler land in the
/// journal of the fixture that started it, where nobody is looking.
@MainActor
final class ScriptedDsScopeTests: XCTestCase {

    override func setUp() {
        super.setUp()
        // ABORT on the first failure: a later assertion running against a
        // fixture that already failed reports a second, derived failure that
        // hides the first.
        continueAfterFailure = false
    }

    /// The property itself, deterministically.
    ///
    /// DISCRIMINATION (RUN, then reverted): with `ScriptedDsURLProtocol` put
    /// back on ONE process-global journal — `seed` ignoring its `scope` and
    /// `rowCount` returning the shared count — this went red:
    ///
    ///     XCTAssertEqual failed: ("2") is not equal to ("1") — a row published
    ///     into another fixture's journal must not be visible here
    func testTwoFixturesJournalsAreIndependent() {
        let stale = ScriptedDsURLProtocol.freshScope()
        let mine = ScriptedDsURLProtocol.freshScope()

        // A straggling publish from a fixture that has already finished.
        ScriptedDsURLProtocol.seed(Data([0x01]), scope: stale)
        // What this fixture actually put on its own journal.
        ScriptedDsURLProtocol.seed(Data([0xDE, 0xAD]), scope: mine)

        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: mine), 1,
            "a row published into another fixture's journal must not be visible here")
        XCTAssertEqual(ScriptedDsURLProtocol.rowCount(scope: stale), 1)
        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: ScriptedDsURLProtocol.freshScope()), 0,
            "a scope nobody has written to is empty, not a view of everything")
    }

    /// The REAL path, so this file pins the hazard rather than only the remedy:
    /// an owner's `bootstrap` really does publish, asynchronously, with nothing
    /// in the test awaiting it — and that publish lands in its own scope only.
    ///
    /// The wait is bounded and its ABSENCE is the assertion that matters: if the
    /// mint never published at all, `observed` stays 0 and the test fails, which
    /// is what stops this passing vacuously if `ensurePushRouting` ever stops
    /// reaching the mint.
    ///
    /// DISCRIMINATION (RUN, then reverted): with `ScriptedDsURLProtocol` put
    /// back on ONE process-global journal, this went red — which is the whole
    /// hazard, demonstrated by execution rather than argued:
    ///
    ///     XCTAssertEqual failed: ("1") is not equal to ("0") - a publish nobody
    ///     awaited must not be countable by another fixture
    func testAnOwnerBootstrapPublishesAsynchronouslyIntoItsOwnScopeOnly() async throws {
        let bystander = ScriptedDsURLProtocol.freshScope()
        let f = try makeOwnerRepo()
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        // Nothing in the fixture awaited the mint, so at this instant the
        // journal is normally still empty — that IS the straggler.
        var observed = 0
        for _ in 0..<60 {
            observed = ScriptedDsURLProtocol.rowCount(scope: f.dsScope)
            if observed > 0 { break }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertGreaterThan(
            observed, 0,
            "the ADR-0042 mint must still publish from `bootstrap`'s fire-and-forget task — "
                + "if it no longer does, this file's premise is stale and the scoping note "
                + "on ScriptedDsURLProtocol needs re-deriving")

        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: bystander), 0,
            "a publish nobody awaited must not be countable by another fixture")
    }

    // MARK: - Fixture

    private struct OwnerFixture {
        let repo: ChatRepository
        let groupIdHex: String
        let accountIdB64: String
        let outboxDir: URL
        let dsScope: String
    }

    /// The smallest fixture that reaches the ADR-0042 mint: a group whose
    /// rehydrated device is the OWNER and holds no routing secret yet.
    private func makeOwnerRepo() throws -> OwnerFixture {
        let dsScope = ScriptedDsURLProtocol.freshScope()

        let ownerSeed = Data(repeating: 0x21, count: 32)
        let owner = try Identity.fromSeed(seed: ownerSeed)
        let group = try Group.create(identity: owner, kind: .group)
        let groupIdHex = DsClient.pseudonymHex(try group.groupId())

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            guard let base = $0.baseAddress else { return }
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, base)
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        let suffix = UUID().uuidString

        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: try owner.snapshotStorage())
        let knownGroupsStore = KnownGroupsStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-scope-k-\(suffix)")))
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("ds-scope-\(suffix)")
        try FileManager.default.createDirectory(
            at: outboxDir, withIntermediateDirectories: true)

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [ScriptedDsURLProtocol.self]
        let ds = DsClient(
            baseURL: ScriptedDsURLProtocol.baseURL(dsScope),
            session: URLSession(configuration: config))
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: Offline.discovery(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: Offline.pushRouting(),
            beaconStore: BeaconObservationStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-scope-b-\(suffix)"))),
            pendingRequestStore: PendingRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-scope-r-\(suffix)"))),
            lastObservedOwnerStore: LastObservedOwnerStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-scope-o-\(suffix)"))),
            leaveRequestStore: LeaveRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-scope-l-\(suffix)"))))
        // Phase 1 fixture: the chats run on the account identity the snapshot
        // was taken under (ADR-0050 device identities are exercised elsewhere).
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: ownerSeed,
                sessionToken: "t",
                sessionExpiresAtMs: 0))

        return OwnerFixture(
            repo: repo, groupIdHex: groupIdHex, accountIdB64: accountIdB64,
            outboxDir: outboxDir, dsScope: dsScope)
    }
}
