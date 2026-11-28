// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

// ADR-0075, amended 2026-10-13 and 2026-11-26 — the account's devices keep
// the recovery address (it moves the email to a new username, and pre-fills
// the form after a recall). The email vault they used to put back with it is
// gone: nothing derived from the phrase is ever sealed for the address.
// Scripted ports only: nothing here reaches a server.

@MainActor
final class RecoveryAddressTests: XCTestCase {

    private func status(enabled: Bool, totp: Bool? = false) -> RecoveryEmailStatus {
        RecoveryEmailStatus(available: true, enabled: enabled, totp: totp)
    }

    private func defaults(_ name: String = #function) -> UserDefaults {
        let suite = "lakko.tests.recovery-address.\(name)"
        UserDefaults().removePersistentDomain(forName: suite)
        return UserDefaults(suiteName: suite)!
    }

    // MARK: - The rules

    /// The email row is absent, off or on; there is no "add again" (it
    /// existed for the vault).
    func testTheEmailRowFollowsTheStatusAlone() {
        XCTAssertEqual(ProtectionRows.decide(status(enabled: true), seals: nil).email, .on)
        XCTAssertEqual(ProtectionRows.decide(status(enabled: false), seals: nil).email, .off)
        XCTAssertEqual(
            ProtectionRows.decide(RecoveryEmailStatus(available: false, enabled: false), seals: nil).email,
            .absent)
        XCTAssertEqual(
            ProtectionRows.decide(RecoveryEmailStatus(available: false, enabled: true), seals: nil).email, .on,
            "an email that is on is shown even where mail is not set up")
    }

    // MARK: - The register (lakko-sync, through the FFI)

    func testTheRegisterIsSetRemovedCarriedAndPersisted() throws {
        let a = SyncedState()
        XCTAssertNil(try a.recoveryEmail())
        XCTAssertNil(try a.recoveryEmailEnvelope())
        XCTAssertTrue(try a.setRecoveryEmail(hlc: 10, address: " Alice@Example.COM "))
        XCTAssertEqual(try a.recoveryEmail(), "alice@example.com", "the core normalises")

        // A sibling device applies the envelope; it is routed by its kind.
        let envelope = try XCTUnwrap(try a.recoveryEmailEnvelope())
        XCTAssertTrue(isRecoveryEmailEnvelope(bytes: envelope))
        XCTAssertFalse(isLineageEnvelope(bytes: envelope))
        let b = SyncedState()
        XCTAssertTrue(try b.applyRecoveryEmailEnvelope(envelope: envelope))
        XCTAssertEqual(try b.recoveryEmail(), "alice@example.com")
        XCTAssertFalse(try b.applyRecoveryEmailEnvelope(envelope: envelope), "idempotent")
        XCTAssertEqual(DeviceGroupSyncRepository.serviceEnvelopeSlot(envelope), "recovery-email")

        // Readable after a restart (the encrypted store keeps `toBytes`),
        // and carried by the backup (`mergeFrom`).
        let restored = try SyncedState.fromBytes(bytes: try a.toBytes())
        XCTAssertEqual(try restored.recoveryEmail(), "alice@example.com")
        let fresh = SyncedState()
        _ = SyncedStateMerge.merge(restored, into: fresh)
        XCTAssertEqual(try fresh.recoveryEmail(), "alice@example.com")

        // Removing writes the empty value, and the removal travels too.
        XCTAssertTrue(try a.setRecoveryEmail(hlc: 20, address: nil))
        XCTAssertNil(try a.recoveryEmail())
        XCTAssertNotNil(try a.recoveryEmailRegister(), "removed at a time, not never written")
        let removal = try XCTUnwrap(try a.recoveryEmailEnvelope())
        XCTAssertTrue(try b.applyRecoveryEmailEnvelope(envelope: removal))
        XCTAssertNil(try b.recoveryEmail())
        // An older write never brings it back.
        XCTAssertFalse(try b.applyRecoveryEmailEnvelope(envelope: envelope))
        XCTAssertNil(try b.recoveryEmail())
        // A refused address is not written.
        XCTAssertThrowsError(try a.setRecoveryEmail(hlc: 30, address: "no-at-sign"))
    }

    // MARK: - The centre: keeping the address

    private final class World {
        var status: RecoveryEmailStatus
        var register: String?
        var syncReady = true
        var stored: [String?] = []
        var statusReads = 0
        init(_ status: RecoveryEmailStatus) { self.status = status }
    }

    private func center(_ world: World) -> AccountProtectionCenter {
        let name = "centre-\(UUID().uuidString)"
        return AccountProtectionCenter(
            ports: .init(
                session: { .init(accountIdB64: "acct", sessionToken: "tok") },
                fetchStatus: { _ in
                    world.statusReads += 1
                    return world.status
                },
                fetchNotices: { _, _ in AccountNoticePage(notices: [], nextAfter: 0, more: false) },
                hasPasskey: { false },
                addressHeld: { world.register },
                storeAddress: { address in
                    guard world.syncReady else { return false }
                    world.stored.append(address)
                    world.register = address
                    return true
                },
                nowMs: { 1_000_000 }),
            store: AccountProtectionStore(defaults: defaults(name)),
            sealsStore: AccountSealsStore(defaults: defaults("seals-" + name)))
    }

    func testTheAddressHeldIsShownAndNothingIsSentForIt() async {
        let world = World(status(enabled: true))
        world.register = "alice@example.com"
        let center = center(world)
        await center.land(accountIdB64: "acct")
        XCTAssertEqual(center.addressHeld, "alice@example.com", "shown in Settings on this device")
        XCTAssertEqual(center.rows?.email, .on)
        XCTAssertEqual(world.stored, [], "a landing writes nothing")
    }

    func testTheAddressIsKeptOnEnrolRemovedOnRemoveAndWaitsForTheSyncedState() async {
        let world = World(status(enabled: false))
        let center = center(world)
        await center.land(accountIdB64: "acct")

        // Enrol finish: the normalised address goes to the register.
        center.enrolled(EmailEnrolled(enabled: true), replaced: false, address: "alice@example.com")
        XCTAssertEqual(center.status?.enabled, true, "the row reads On at once")
        await center.flushPendingAddress()
        XCTAssertEqual(world.stored, ["alice@example.com"])
        // Replace: the register takes the new address.
        center.enrolled(EmailEnrolled(enabled: true), replaced: true, address: "new@example.com")
        await center.flushPendingAddress()
        XCTAssertEqual(world.register, "new@example.com")
        // Removing the recovery email writes the empty value.
        center.removed()
        await center.flushPendingAddress()
        XCTAssertNil(world.register)
        XCTAssertEqual(world.stored.last ?? "x", nil)
        XCTAssertEqual(center.status?.enabled, false)

        // Got back in by email before the synced state is loaded: the write
        // waits for it.
        let late = World(status(enabled: true))
        late.syncReady = false
        let fresh = self.center(late)
        fresh.keepAddress("alice@example.com")
        await fresh.land(accountIdB64: "acct")
        XCTAssertEqual(late.stored, [])
        late.syncReady = true
        await fresh.flushPendingAddress()
        XCTAssertEqual(late.stored, ["alice@example.com"])
        await fresh.flushPendingAddress()
        XCTAssertEqual(late.stored.count, 1, "written once")
    }

    /// ADR-0076 section 2 / UX 19.1c — what a username change carries for
    /// the recovery email, and when the screen asks for the address.
    func testAUsernameChangeCarriesTheAddressOrAsksForIt() {
        XCTAssertEqual(UsernameRename.emailCarry(recoveryEmailEnabled: true, addressHeld: true), .address)
        XCTAssertEqual(UsernameRename.emailCarry(recoveryEmailEnabled: true, addressHeld: false), .none)
        XCTAssertEqual(UsernameRename.emailCarry(recoveryEmailEnabled: false, addressHeld: true), .none)

        let emailOnly = AccountFactors(email: true, authenticator: false, passkey: false)
        // The email is the last second way in and no address is here: the
        // change would be refused, so the address is asked for first.
        XCTAssertTrue(UsernameRename.asksForAddress(factors: emailOnly, addressHeld: false))
        XCTAssertFalse(UsernameRename.asksForAddress(factors: emailOnly, addressHeld: true))
        // Another way in remains: the email may be dropped (with its warning).
        XCTAssertFalse(UsernameRename.asksForAddress(
            factors: AccountFactors(email: true, authenticator: true, passkey: false), addressHeld: false))
        XCTAssertFalse(UsernameRename.asksForAddress(
            factors: AccountFactors(email: false, authenticator: true, passkey: false), addressHeld: false))
        XCTAssertFalse(UsernameRename.asksForAddress(factors: nil, addressHeld: false), "unknown: not asked")

        // The bind's body names the address only.
        let json = try? JSONSerialization.jsonObject(
            with: JSONEncoder().encode(LoginHandleRecoveryEmail(address: "a@b.example"))) as? [String: Any]
        XCTAssertEqual(json.map { Set($0.keys) }, ["address"])
    }
}
