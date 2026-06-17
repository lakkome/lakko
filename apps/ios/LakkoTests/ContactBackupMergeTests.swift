// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// Unit tests for the contact-in-backup restore merge
/// (``reconstructRestoredContacts``, ADR-0021). The helper +
/// `BackupContactRecord` / `AcceptedContact` / `IncomingContactRequest` are
/// plain value types, so these run without the Keychain / UserDefaults.
final class ContactBackupMergeTests: XCTestCase {
    private func record(_ edByte: UInt8, _ name: String, _ accountId: Data?)
        -> BackupContactRecord
    {
        BackupContactRecord(
            ed25519Pubkey: Data(repeating: edByte, count: 32),
            x25519Pubkey: Data(count: 32),
            displayName: name,
            accountId: accountId
        )
    }

    func testReconstructsWithAccountIdAndName() {
        let accountId = Data(repeating: 0xAB, count: 16)
        let out = reconstructRestoredContacts(
            existing: [],
            restored: [record(0x01, "Alice", accountId)],
            createdAtMs: 1_700_000_000_000
        )
        XCTAssertEqual(out.count, 1)
        XCTAssertEqual(out[0].displayName, "Alice")
        XCTAssertEqual(out[0].request.fromAccountId, accountId)
        XCTAssertEqual(out[0].request.fromPubkey, Data(repeating: 0x01, count: 32))
        XCTAssertEqual(out[0].request.validAfterMs, 1_700_000_000_000)
    }

    func testEmptyNameBecomesNil() {
        let out = reconstructRestoredContacts(
            existing: [],
            restored: [record(0x02, "", nil)],
            createdAtMs: 1
        )
        XCTAssertNil(out[0].displayName, "an empty backup name must not become a chosen name")
    }

    func testNilAccountIdStaysNil() {
        let out = reconstructRestoredContacts(
            existing: [],
            restored: [record(0x03, "Bob", nil)],
            createdAtMs: 1
        )
        XCTAssertNil(out[0].request.fromAccountId)
    }

    func testDedupSkipsExistingPubkey() {
        let existingPubkey = Data(repeating: 0x04, count: 32)
        let existing = [
            AcceptedContact(
                request: IncomingContactRequest(
                    messageId: 42,
                    fromPubkey: existingPubkey,
                    fromAccountId: nil,
                    fromPseudonym: Data(count: 32),
                    validAfterMs: 0,
                    intro: nil,
                    createdAt: ""
                ),
                groupIdHex: "abcd",
                displayName: "Carol (live)"
            )
        ]
        let out = reconstructRestoredContacts(
            existing: existing,
            restored: [record(0x04, "Carol (stale backup)", nil), record(0x05, "Dave", nil)],
            createdAtMs: 1
        )
        XCTAssertEqual(out.count, 1, "only the new pubkey is reconstructed")
        XCTAssertEqual(out[0].displayName, "Dave")
    }

    func testSentinelMessageIdsNegativeAndDistinct() {
        let out = reconstructRestoredContacts(
            existing: [],
            restored: [record(0x06, "A", nil), record(0x07, "B", nil), record(0x08, "C", nil)],
            createdAtMs: 1
        )
        let ids = out.map { $0.request.messageId }
        XCTAssertEqual(ids.count, 3)
        XCTAssertTrue(ids.allSatisfy { $0 < 0 }, "all sentinel ids are negative")
        XCTAssertEqual(Set(ids).count, 3, "sentinel ids are distinct")
    }
}
