// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import XCTest
@testable import Lakko

/// ADR-0041 Phase 1 — tests for the pure App Group storage-migration
/// decision logic and the two migrations that are tractable without a
/// provisioned device / entitlement (the filesystem copy + the
/// `UserDefaults`-backed known-groups list).
///
/// The Keychain access-group operations (seed + wrap keys) cannot be
/// exercised deterministically on the simulator — access-group
/// enforcement is a device/provisioning property — so those are covered
/// by the pure decision primitives (`shouldCopy` / `effectiveValue`) that
/// the store migrations are built on, plus the real filesystem +
/// `UserDefaults` migrations below.
final class SharedStorageMigrationTests: XCTestCase {

    // MARK: - shouldCopy decision matrix (copy-if-absent, copy-not-move)

    func testShouldCopy_newAbsentOldPresent_copies() {
        XCTAssertTrue(SharedStorage.shouldCopy(sharedPresent: false, oldPresent: true))
    }

    func testShouldCopy_bothPresent_noop() {
        XCTAssertFalse(SharedStorage.shouldCopy(sharedPresent: true, oldPresent: true))
    }

    func testShouldCopy_newPresent_noop() {
        // Shared present (old absent) → already migrated / fresh install.
        XCTAssertFalse(SharedStorage.shouldCopy(sharedPresent: true, oldPresent: false))
    }

    func testShouldCopy_bothAbsent_noop() {
        XCTAssertFalse(SharedStorage.shouldCopy(sharedPresent: false, oldPresent: false))
    }

    // MARK: - effectiveValue read-with-fallback (incl. copy-failure)

    func testEffectiveValue_newPresent_readsNew() {
        XCTAssertEqual(SharedStorage.effectiveValue(shared: "new", old: "old"), "new")
    }

    func testEffectiveValue_bothPresent_prefersNew() {
        XCTAssertEqual(SharedStorage.effectiveValue(shared: "new", old: "old"), "new")
    }

    func testEffectiveValue_copyFailure_fallsBackToOld() {
        // A failed copy leaves the shared value nil → the reader falls
        // back to the old value (never stranded).
        XCTAssertEqual(SharedStorage.effectiveValue(shared: String?.none, old: "old"), "old")
    }

    func testEffectiveValue_bothAbsent_nil() {
        XCTAssertNil(SharedStorage.effectiveValue(shared: String?.none, old: String?.none))
    }

    // MARK: - migrateFileIfNeeded (real filesystem, copy-not-move)

    /// new-absent + old-present → copies to shared, LEAVES old in place,
    /// returns the shared URL, and the bytes match.
    func testMigrateFile_newAbsentOldPresent_copiesAndKeepsOld() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        let payload = Data("mls-snapshot-bytes".utf8)
        try payload.write(to: old)

        let readURL = SharedStorage.migrateFileIfNeeded(old: old, shared: shared)

        XCTAssertEqual(readURL, shared, "reader should now use the shared copy")
        XCTAssertTrue(FileManager.default.fileExists(atPath: shared.path),
                      "shared copy must exist after migration")
        XCTAssertTrue(FileManager.default.fileExists(atPath: old.path),
                      "copy-not-move: the old copy must NOT be deleted")
        XCTAssertEqual(try Data(contentsOf: shared), payload,
                       "shared copy must be byte-identical to the original")
    }

    /// both-present → no-op: the shared copy is NOT overwritten by the old
    /// copy, and the reader keeps using the shared copy.
    func testMigrateFile_bothPresent_noop() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        try Data("stale-old".utf8).write(to: old)
        let sharedBytes = Data("authoritative-shared".utf8)
        try FileManager.default.createDirectory(
            at: shared.deletingLastPathComponent(), withIntermediateDirectories: true)
        try sharedBytes.write(to: shared)

        let readURL = SharedStorage.migrateFileIfNeeded(old: old, shared: shared)

        XCTAssertEqual(readURL, shared)
        XCTAssertEqual(try Data(contentsOf: shared), sharedBytes,
                       "an already-present shared copy must never be clobbered by the old copy")
    }

    /// new-present + old-absent → returns the shared URL.
    func testMigrateFile_newPresentOldAbsent_readsNew() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        try FileManager.default.createDirectory(
            at: shared.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("only-shared".utf8).write(to: shared)

        let readURL = SharedStorage.migrateFileIfNeeded(old: old, shared: shared)

        XCTAssertEqual(readURL, shared)
        XCTAssertFalse(FileManager.default.fileExists(atPath: old.path))
    }

    /// both-absent → nil (nothing to read).
    func testMigrateFile_bothAbsent_nil() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        XCTAssertNil(SharedStorage.migrateFileIfNeeded(old: old, shared: shared))
    }

    /// Idempotent — a second migration is a no-op and preserves the bytes.
    func testMigrateFile_idempotent() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        let payload = Data("idempotent".utf8)
        try payload.write(to: old)

        _ = SharedStorage.migrateFileIfNeeded(old: old, shared: shared)
        let readURL = SharedStorage.migrateFileIfNeeded(old: old, shared: shared)

        XCTAssertEqual(readURL, shared)
        XCTAssertEqual(try Data(contentsOf: shared), payload)
        XCTAssertTrue(FileManager.default.fileExists(atPath: old.path))
    }

    // MARK: - KnownGroupsStore migration (real UserDefaults suites)

    /// standard-present + suite-empty → migrate copies the list into the
    /// suite, LEAVES standard in place, and reads then prefer the suite.
    func testKnownGroups_migrateCopiesAndPrefersSuite() throws {
        let (suite, standard, acct, cleanup) = makeDefaultsPair()
        defer { cleanup() }
        let store = KnownGroupsStore(sharedDefaults: suite, standardDefaults: standard)
        // Seed the OLD location directly (simulate a pre-update install).
        standard.set(["aa", "bb"], forKey: "me.lakko.known-groups.v1.\(acct)")

        // Read before migration falls back to standard.
        XCTAssertEqual(store.load(accountIdB64: acct), ["aa", "bb"])

        store.migrateToSharedSuite(accountIdB64: acct)

        XCTAssertEqual(
            Set(suite.stringArray(forKey: "me.lakko.known-groups.v1.\(acct)") ?? []),
            ["aa", "bb"], "suite must hold the migrated list")
        XCTAssertNotNil(
            standard.stringArray(forKey: "me.lakko.known-groups.v1.\(acct)"),
            "copy-not-move: the standard list must NOT be deleted")
        XCTAssertEqual(store.load(accountIdB64: acct), ["aa", "bb"])
    }

    /// Idempotent + never-clobber: a suite that already differs from
    /// standard is not overwritten by a re-run.
    func testKnownGroups_migrateIsIdempotentAndNeverClobbers() throws {
        let (suite, standard, acct, cleanup) = makeDefaultsPair()
        defer { cleanup() }
        let store = KnownGroupsStore(sharedDefaults: suite, standardDefaults: standard)
        let key = "me.lakko.known-groups.v1.\(acct)"
        standard.set(["aa"], forKey: key)
        suite.set(["cc", "dd"], forKey: key) // suite already authoritative

        store.migrateToSharedSuite(accountIdB64: acct) // must be a no-op

        XCTAssertEqual(Set(suite.stringArray(forKey: key) ?? []), ["cc", "dd"],
                       "an already-present suite list must never be clobbered")
        XCTAssertEqual(store.load(accountIdB64: acct), ["cc", "dd"],
                       "reads prefer the suite")
    }

    /// add writes to the suite; clear wipes BOTH locations.
    func testKnownGroups_addWritesSuite_clearWipesBoth() throws {
        let (suite, standard, acct, cleanup) = makeDefaultsPair()
        defer { cleanup() }
        let store = KnownGroupsStore(sharedDefaults: suite, standardDefaults: standard)
        let key = "me.lakko.known-groups.v1.\(acct)"
        standard.set(["legacy"], forKey: key) // stale old copy

        store.add(accountIdB64: acct, groupIdHex: "new")

        XCTAssertEqual(Set(suite.stringArray(forKey: key) ?? []), ["legacy", "new"],
                       "add reads the effective set (fallback) then writes the suite")

        store.clear(accountIdB64: acct)
        XCTAssertNil(suite.stringArray(forKey: key))
        XCTAssertNil(standard.stringArray(forKey: key), "clear wipes the old copy too")
    }

    /// both-absent → empty read + migrate no-op.
    func testKnownGroups_bothAbsent() throws {
        let (suite, standard, acct, cleanup) = makeDefaultsPair()
        defer { cleanup() }
        let store = KnownGroupsStore(sharedDefaults: suite, standardDefaults: standard)
        XCTAssertEqual(store.load(accountIdB64: acct), [])
        store.migrateToSharedSuite(accountIdB64: acct)
        XCTAssertNil(suite.stringArray(forKey: "me.lakko.known-groups.v1.\(acct)"))
    }

    // MARK: - ADR-0041 LOW-1: atomic migration (no truncated shared file)

    /// A large source migrates byte-identically AND the staged temp file is
    /// gone afterwards — the shared directory holds ONLY the finished shared
    /// file, no `.migrate-*.tmp` residue. The atomic rename means the shared
    /// path only ever appears fully-formed; a partial file is never left.
    func testMigrateFile_largeSource_atomicNoTempResidue() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        // 2 MiB so the copy is not a single-syscall trivial op.
        var payload = Data(count: 2 * 1024 * 1024)
        payload.withUnsafeMutableBytes { raw in
            for i in stride(from: 0, to: raw.count, by: 7) { raw[i] = UInt8(i & 0xFF) }
        }
        try payload.write(to: old)

        let readURL = SharedStorage.migrateFileIfNeeded(old: old, shared: shared)

        XCTAssertEqual(readURL, shared)
        XCTAssertEqual(try Data(contentsOf: shared), payload,
                       "shared copy must be byte-identical to the (large) original")
        XCTAssertTrue(FileManager.default.fileExists(atPath: old.path),
                      "copy-not-move: the old copy must NOT be deleted")
        XCTAssertEqual(try tempResidue(in: shared.deletingLastPathComponent()), [],
                       "no staged temp file may survive a completed migration")
    }

    /// Retrying the migration (idempotent) leaves the shared file
    /// byte-identical and still leaves no temp residue behind.
    func testMigrateFile_retried_noTempResidueAndByteIdentical() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        let payload = Data("mls-snapshot-bytes-retry".utf8)
        try payload.write(to: old)

        _ = SharedStorage.migrateFileIfNeeded(old: old, shared: shared)
        _ = SharedStorage.migrateFileIfNeeded(old: old, shared: shared) // retry (no-op)

        XCTAssertEqual(try Data(contentsOf: shared), payload)
        XCTAssertEqual(try tempResidue(in: shared.deletingLastPathComponent()), [],
                       "a retried migration must not accumulate temp residue")
    }

    // MARK: - ADR-0041 LOW-1: read fallback on OPEN failure (defence in depth)

    /// Models `IdentitySnapshotStore.getSnapshot` / `MessageLogStore.getLog`:
    /// a TRUNCATED shared (preferred) envelope + an INTACT old envelope →
    /// the reader falls THROUGH the failed shared open to the old copy and
    /// recovers the plaintext, instead of shadowing it with `nil`.
    func testReadFallback_truncatedShared_opensOld() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        let key = SymmetricKey(size: .bits256)
        let plaintext = Data("irreplaceable-mls-snapshot".utf8)
        try writeSealed(plaintext, key: key, to: old)
        // A truncated envelope at the PREFERRED (shared) path: fails to open.
        try ensureParentDir(shared)
        try Data("not-a-valid-gcm-envelope".utf8).write(to: shared)

        let recovered = SharedStorage.firstReadable([shared, old]) {
            openEnvelope(at: $0, key: key)
        }

        XCTAssertEqual(recovered, plaintext,
                       "a truncated preferred copy must fall through to the intact old copy")
    }

    /// prefer-shared ordering is preserved: when BOTH copies open, the
    /// shared (preferred) plaintext wins — the fallback must not regress the
    /// normal steady-state read.
    func testReadFallback_bothOpen_prefersShared() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        let key = SymmetricKey(size: .bits256)
        try writeSealed(Data("old-plaintext".utf8), key: key, to: old)
        try ensureParentDir(shared)
        try writeSealed(Data("shared-plaintext".utf8), key: key, to: shared)

        let recovered = SharedStorage.firstReadable([shared, old]) {
            openEnvelope(at: $0, key: key)
        }

        XCTAssertEqual(recovered, Data("shared-plaintext".utf8),
                       "when both open, the preferred shared copy wins")
    }

    /// Fail-closed: BOTH copies unopenable → `nil` (never a plaintext guess).
    func testReadFallback_bothCorrupt_nil() throws {
        let (old, shared, cleanup) = try makeFilePair()
        defer { cleanup() }
        let key = SymmetricKey(size: .bits256)
        try Data("garbage-old".utf8).write(to: old)
        try ensureParentDir(shared)
        try Data("garbage-shared".utf8).write(to: shared)

        let recovered = SharedStorage.firstReadable([shared, old]) {
            openEnvelope(at: $0, key: key)
        }

        XCTAssertNil(recovered, "both-corrupt must fail closed (nil)")
    }

    /// Fail-closed: no candidates at all → `nil`.
    func testReadFallback_noCandidates_nil() {
        let recovered = SharedStorage.firstReadable([URL]()) {
            openEnvelope(at: $0, key: SymmetricKey(size: .bits256))
        }
        XCTAssertNil(recovered, "no candidates must fail closed (nil)")
    }

    // MARK: - Helpers

    /// Names of any lingering staged temp files (`.migrate-*.tmp`) in `dir`.
    /// The temp name starts with a dot, so hidden files must NOT be skipped.
    private func tempResidue(in dir: URL) throws -> [String] {
        let entries = try FileManager.default.contentsOfDirectory(
            at: dir, includingPropertiesForKeys: nil, options: []
        )
        return entries
            .map { $0.lastPathComponent }
            .filter { $0.contains(".migrate-") || $0.hasSuffix(".tmp") }
            .sorted()
    }

    /// Seal `plaintext` under `key` and write the RFC-5116 `combined`
    /// envelope to `url`, creating the parent directory.
    private func writeSealed(_ plaintext: Data, key: SymmetricKey, to url: URL) throws {
        try ensureParentDir(url)
        let combined = try XCTUnwrap(try AES.GCM.seal(plaintext, using: key).combined)
        try combined.write(to: url)
    }

    /// Ensure the parent directory of `url` exists (the `makeFilePair`
    /// shared dir is created lazily by the migration in production).
    private func ensureParentDir(_ url: URL) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    }

    /// The exact open path `getSnapshot` uses: read → SealedBox → GCM-open,
    /// `nil` on any absent / truncated / auth failure.
    private func openEnvelope(at url: URL, key: SymmetricKey) -> Data? {
        guard let combined = try? Data(contentsOf: url) else { return nil }
        guard let sealed = try? AES.GCM.SealedBox(combined: combined),
              let plaintext = try? AES.GCM.open(sealed, using: key) else {
            return nil
        }
        return plaintext
    }

    private func makeFilePair() throws -> (old: URL, shared: URL, cleanup: () -> Void) {
        let base = FileManager.default.temporaryDirectory
            .appendingPathComponent("adr0041-\(UUID().uuidString)")
        let oldDir = base.appendingPathComponent("old")
        let sharedDir = base.appendingPathComponent("shared")
        try FileManager.default.createDirectory(at: oldDir, withIntermediateDirectories: true)
        let old = oldDir.appendingPathComponent("acct.bin")
        let shared = sharedDir.appendingPathComponent("acct.bin")
        return (old, shared, { try? FileManager.default.removeItem(at: base) })
    }

    private func makeDefaultsPair() -> (
        suite: UserDefaults, standard: UserDefaults, acct: String, cleanup: () -> Void
    ) {
        let suiteName = "adr0041.suite.\(UUID().uuidString)"
        let stdName = "adr0041.std.\(UUID().uuidString)"
        // Two transient suites stand in for the shared suite + `.standard`
        // so the test never touches the real process defaults.
        let suite = UserDefaults(suiteName: suiteName)!
        let standard = UserDefaults(suiteName: stdName)!
        let acct = "AAAABBBBCCCCDDDD=="
        return (suite, standard, acct, {
            suite.removePersistentDomain(forName: suiteName)
            standard.removePersistentDomain(forName: stdName)
        })
    }
}
