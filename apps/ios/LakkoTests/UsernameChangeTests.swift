// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

// ADR-0076 — changing a username: the pure rules. The flow over a scripted
// server is in `RecoveryFlowTests`.

@MainActor
final class UsernameChangeTests: XCTestCase {

    // MARK: - The session's handle sync

    func testTheHandleSyncNeverBindsOverAnotherName() {
        XCTAssertEqual(HandleSync.action(.same), .nothing)
        XCTAssertEqual(HandleSync.action(.none), .bind, "a first bind")
        XCTAssertEqual(HandleSync.action(.other), .waitForRegister, "renamed elsewhere: bind nothing")
        XCTAssertEqual(LoginHandleState(rawValue: "same"), .same)
        XCTAssertEqual(LoginHandleState(rawValue: "other"), .other)
        XCTAssertEqual(LoginHandleState(rawValue: "none"), LoginHandleState.none)
        XCTAssertNil(LoginHandleState(rawValue: "something"))
    }

    // MARK: - The own-username register

    func testWhatADeviceDoesWithTheRegister() {
        typealias R = OwnUsernameRule
        // An existing account, an empty register: seeded once from the marker.
        XCTAssertEqual(R.action(local: "alice", register: nil, pending: nil, handleConfirmed: true), .seed("alice"))
        XCTAssertEqual(R.action(local: "alice", register: "", pending: nil, handleConfirmed: true), .seed("alice"))
        // A guest has nothing to seed.
        XCTAssertEqual(R.action(local: "", register: nil, pending: nil, handleConfirmed: true), .nothing)
        XCTAssertEqual(R.action(local: "alice", register: "alice", pending: nil, handleConfirmed: true), .nothing)
        // A name written elsewhere differs: adopt it (a guest whose sibling claimed one too).
        XCTAssertEqual(R.action(local: "alice", register: "alice_b", pending: nil, handleConfirmed: true), .adopt("alice_b"))
        XCTAssertEqual(R.action(local: "", register: "alice", pending: nil, handleConfirmed: true), .adopt("alice"))
        // This device's own write is still owed: the register holds the name
        // it just replaced, which must not be adopted back.
        XCTAssertEqual(R.action(local: "alice_b", register: "alice", pending: "alice_b", handleConfirmed: true), .writePending("alice_b"))
        XCTAssertEqual(R.action(local: "alice_b", register: nil, pending: "alice_b", handleConfirmed: true), .writePending("alice_b"))
        XCTAssertEqual(R.action(local: "alice_b", register: "alice_b", pending: "alice_b", handleConfirmed: true), .nothing)
    }

    /// A device that has not yet received a rename holds the old name. If it
    /// seeded an empty register, its stamp would be the newest and the
    /// rename would be undone everywhere.
    func testAStaleDeviceDoesNotSeed() {
        typealias R = OwnUsernameRule
        // The check answered `other`, or has not answered: wait for the register.
        XCTAssertEqual(R.action(local: "alice", register: nil, pending: nil, handleConfirmed: false), .nothing)
        XCTAssertEqual(R.action(local: "alice", register: "", pending: nil, handleConfirmed: false), .nothing)
        // Adopting a name written elsewhere needs no confirmation.
        XCTAssertEqual(
            R.action(local: "alice", register: "alice_b", pending: nil, handleConfirmed: false), .adopt("alice_b"))
        // Nor does this device's own owed write (it made the change itself).
        XCTAssertEqual(
            R.action(local: "alice_b", register: nil, pending: "alice_b", handleConfirmed: false),
            .writePending("alice_b"))
    }

    func testTheRegisterThroughTheCore() throws {
        let a = SyncedState()
        XCTAssertNil(try a.ownUsername())
        XCTAssertTrue(try a.setOwnUsername(hlc: 10, username: "alice"))
        XCTAssertEqual(try a.ownUsername(), "alice")
        let envelope = try XCTUnwrap(try a.ownUsernameEnvelope())
        XCTAssertTrue(isOwnUsernameEnvelope(bytes: envelope))
        XCTAssertFalse(isRecoveryEmailEnvelope(bytes: envelope))
        XCTAssertEqual(DeviceGroupSyncRepository.serviceEnvelopeSlot(envelope), "own-username")

        // A sibling applies it; a newer name wins; an older one never returns.
        let b = SyncedState()
        XCTAssertTrue(try b.applyOwnUsernameEnvelope(envelope: envelope))
        XCTAssertTrue(try a.setOwnUsername(hlc: 20, username: "alice_b"))
        let newer = try XCTUnwrap(try a.ownUsernameEnvelope())
        XCTAssertTrue(try b.applyOwnUsernameEnvelope(envelope: newer))
        XCTAssertEqual(try b.ownUsername(), "alice_b")
        XCTAssertFalse(try b.applyOwnUsernameEnvelope(envelope: envelope))
        XCTAssertEqual(try b.ownUsername(), "alice_b")

        // Persisted and backed up with the rest of the state.
        let restored = try SyncedState.fromBytes(bytes: try a.toBytes())
        XCTAssertEqual(try restored.ownUsername(), "alice_b")
        let fresh = SyncedState()
        _ = SyncedStateMerge.merge(restored, into: fresh)
        XCTAssertEqual(try fresh.ownUsername(), "alice_b")
    }

    // MARK: - The rename

    func testWhatTheScreenShowsForTheRecoveryEmail() {
        // The device holds the address: the email is kept, no warning.
        XCTAssertFalse(UsernameRename.warnings(recoveryEmailEnabled: true, addressHeld: true).recoveryEmailTurnedOff)
        // It does not: the rename turns the recovery email off, and says so.
        XCTAssertTrue(UsernameRename.warnings(recoveryEmailEnabled: true, addressHeld: false).recoveryEmailTurnedOff)
        // No recovery email: nothing to warn about.
        XCTAssertFalse(UsernameRename.warnings(recoveryEmailEnabled: false, addressHeld: false).recoveryEmailTurnedOff)
        XCTAssertFalse(UsernameRename.warnings(recoveryEmailEnabled: false, addressHeld: true).recoveryEmailTurnedOff)

    }

    func testWhatTheBindCarriesForTheRecoveryEmail() {
        typealias U = UsernameRename
        XCTAssertEqual(U.emailCarry(recoveryEmailEnabled: false, addressHeld: true, phraseSaved: false), .none)
        XCTAssertEqual(U.emailCarry(recoveryEmailEnabled: true, addressHeld: false, phraseSaved: false), .none)
        // A vault only while no phrase is saved.
        XCTAssertEqual(
            U.emailCarry(recoveryEmailEnabled: true, addressHeld: true, phraseSaved: false), .addressAndVault)
        XCTAssertEqual(U.emailCarry(recoveryEmailEnabled: true, addressHeld: true, phraseSaved: true), .addressOnly)

        // The wire: the grant, the previous handle, the re-key.
        let body = LoginHandleBindRequest(
            handle: "new", freshGrant: "g", previousHandle: "old",
            recoveryEmail: LoginHandleRecoveryEmail(
                address: "a@b.example", vaultSalt: Data(repeating: 1, count: 16),
                sealedEntropy: Data(repeating: 2, count: 44)))
        let json = (try? JSONSerialization.jsonObject(with: JSONEncoder().encode(body))) as? [String: Any]
        XCTAssertEqual(json?["fresh_grant"] as? String, "g")
        XCTAssertEqual(json?["previous_handle"] as? String, "old")
        let email = json?["recovery_email"] as? [String: Any]
        XCTAssertEqual(email?["address"] as? String, "a@b.example")
        XCTAssertNotNil(email?["vault_salt"])
        XCTAssertNotNil(email?["sealed_entropy"])
        // A first bind sends the handle alone.
        let first = (try? JSONSerialization.jsonObject(
            with: JSONEncoder().encode(LoginHandleBindRequest(handle: "h")))) as? [String: Any]
        XCTAssertEqual(first.map { Set($0.keys) }, ["handle"])
    }

    func testTheRenameStepsAndTheRollback() {
        typealias U = UsernameRename
        func of(_ status: Int, _ body: String) -> U.ReplaceOutcome { .of(status: status, body: body) }
        XCTAssertEqual(of(200, "{}"), .replaced)
        XCTAssertEqual(of(409, #"{"error":"username-taken"}"#), .taken)
        // No rename limit any more (ADR-0076 amended 2026-10-26): a 429 is
        // simply a failure.
        XCTAssertEqual(of(429, #"{"error":"rename-too-soon"}"#), .failed)
        XCTAssertEqual(of(500, ""), .failed)
        XCTAssertEqual(
            U.ReplaceOutcome.of(DiscoveryClientError.network(endpoint: "/r", underlying: URLError(.timedOut))),
            .unknown)

        // Discovery answered: finish. It refused: the old handle back. No
        // answer: keep the record, the next landing asks again.
        XCTAssertEqual(U.afterReplace(.replaced), .finalize)
        XCTAssertEqual(U.afterReplace(.taken), .rollBack)
        XCTAssertEqual(U.afterReplace(.failed), .rollBack)
        XCTAssertEqual(U.afterReplace(.unknown), .keepOwed)

        XCTAssertEqual(U.failure(.taken, nowMs: 0), .taken)
        XCTAssertEqual(U.failure(.unknown, nowMs: 0), .offline)
        // A rollback changes the handle, so it needs the grant.
        XCTAssertTrue(U.canRollBack(holdsGrant: true))
        XCTAssertFalse(U.canRollBack(holdsGrant: false))
    }

    func testTheOwedRecordSaysWhereALandingGoesOn() throws {
        typealias U = UsernameRename
        func record(_ phase: U.Record.Phase) -> U.Record {
            U.Record(oldName: "alice", newName: "alice_b", phase: phase, startedAtMs: 7)
        }
        XCTAssertEqual(U.resume(nil), .nothing)
        XCTAssertEqual(U.resume(record(.binding)), .checkHandle)
        XCTAssertEqual(U.resume(record(.bound)), .replace)
        XCTAssertEqual(U.resume(record(.replaced)), .finalize)
        // A bind whose answer was lost: the new handle is bound (go on) or
        // it is not (nothing changed).
        XCTAssertEqual(U.afterBindingCheck(newHandle: .same), .replace)
        XCTAssertEqual(U.afterBindingCheck(newHandle: .other), .nothing)
        XCTAssertEqual(U.afterBindingCheck(newHandle: .none), .nothing)

        // The record survives a restart, per account, and the wipe takes it.
        let suite = "lakko.tests.username-change.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { UserDefaults().removePersistentDomain(forName: suite) }
        let store = AccountProtectionStore(defaults: defaults)
        XCTAssertNil(store.renameOwed(accountIdB64: "acct"))
        store.setRenameOwed(record(.bound), accountIdB64: "acct")
        store.setUsernameRegisterOwed("alice_b", accountIdB64: "acct")
        XCTAssertEqual(store.renameOwed(accountIdB64: "acct"), record(.bound))
        XCTAssertNil(store.renameOwed(accountIdB64: "other"))
        XCTAssertEqual(store.usernameRegisterOwed(accountIdB64: "acct"), "alice_b")
        store.clear(accountIdB64: "acct")
        XCTAssertNil(store.renameOwed(accountIdB64: "acct"))
        XCTAssertNil(store.usernameRegisterOwed(accountIdB64: "acct"))
    }

    func testTheStringsAndTheProofSurface() {
        for key in [
            "proof.username.help", "recovery_email.username_change",
        ] {
            XCTAssertNotEqual(Localization.text(key), key, "\(key) is missing from the catalog")
        }
        XCTAssertEqual(FreshAuthSurface.changeUsername.helpKey, "proof.username.help")
    }
}
