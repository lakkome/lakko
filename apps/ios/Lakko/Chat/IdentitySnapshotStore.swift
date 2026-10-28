// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import Security

/// Per-account on-disk persistence for the MLS provider snapshot
/// blob produced by `Identity.snapshotStorage()`.
///
/// Mirror of:
///
///   * `apps/web/src/chat/mls-store.ts` (1.17f5)
///   * `apps/android/.../chat/IdentitySnapshotStore.kt` (1.15f3)
///
/// ## Threat model
///
/// The snapshot blob is the most sensitive bytes any Lakko process
/// touches — it carries every group's ratchet state, the
/// leaf-secret material, and the init-key private bytes. A dump
/// of the raw blob would let an attacker decrypt every prior
/// message in every group the user is part of. The blob must be
/// wrapped under a hardware-isolated key — same posture as
/// ``DeviceKeyStore`` from 1.14d2.
///
/// ## Why a file + Keychain-wrapped AES rather than direct
/// Keychain storage
///
/// iOS Keychain has a per-item size ceiling around 4-8 KB on
/// `kSecValueData`. MLS snapshot blobs grow with group count + key
/// material — easily 10+ KB once a user has half a dozen 1×1
/// chats. We use the same envelope pattern Android takes:
///
///   1. A 32-byte AES-256 **wrap key** lives in the Keychain under
///      `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — same
///      class-key-protection profile as the device root key, so
///      the wrap key never leaves Secure Enclave-derived storage.
///   2. The snapshot blob is encrypted with AES-256-GCM using the
///      wrap key + a fresh per-write 12-byte IV via CryptoKit's
///      `AES.GCM.seal(_:using:)`.
///   3. The opaque `(iv || ciphertext || tag)` envelope is
///      persisted to a per-account file under
///      `Application Support/lakko-mls-snapshots/<accountIdB64>.bin`,
///      excluded from iCloud backup via the
///      `URLResourceKey.isExcludedFromBackupKey` flag.
///
/// `Application Support` (not `Documents`) is the canonical
/// iOS-idiomatic location for "app-owned, never user-visible"
/// state — same posture as Android's
/// `Context.noBackupFilesDir`. Exclusion from iCloud backup is
/// critical: MLS state is device-bound; an auto-restore from
/// iCloud must NOT carry it between devices, even if the user
/// opts in — the recipient peer's view of the conversation would
/// diverge from the restored sender's view the moment any of
/// them sent a new message.
///
/// ## Persistence layout
///
/// ```text
///   ApplicationSupport/lakko-mls-snapshots/
///     <accountIdB64>.bin     12-byte IV || ciphertext || 16-byte tag
/// ```
public final class IdentitySnapshotStore {

    /// Keychain service tag for the AES wrap key.
    public static let wrapKeyService: String = "me.lakko.mls-snapshot-wrap.v1"

    /// Keychain account marker for the wrap key. One wrap key per
    /// app installation (not per Lakko account) — the per-account
    /// isolation comes from the file path, not the wrap key.
    /// Mirror of Android's `WRAP_KEY_ALIAS` — one alias, every
    /// account's snapshot wraps under the same key.
    private static let wrapKeyAccount: String = "wrap-key"

    /// Container subdirectory holding the per-account envelopes.
    private static let subdir: String = "lakko-mls-snapshots"

    /// Old app-private location (`Application Support/lakko-mls-snapshots`).
    /// Retained as the read-fallback + migration source per ADR-0041 —
    /// never deleted by the migration.
    private let oldEnvelopeDir: URL?

    /// A suffix of the record name, so one account can keep more than one
    /// provider snapshot: the chat provider has none, the channel publisher
    /// provider (``ChannelPublisherIdentity``) has `.channels`. The account
    /// store gate is entered under the account id alone.
    private let scope: String

    public init(scope: String = "") {
        self.oldEnvelopeDir = Self.oldEnvelopeDir()
        self.scope = scope
    }

    // MARK: - Public API

    /// Persist `blob` for `accountIdB64`. Overwrites any existing
    /// envelope. Best-effort — Keychain or filesystem failures
    /// surface as a logged-and-swallowed no-op at the
    /// ``ChatRepository`` call site.
    ///
    /// ADR-0041 — the authoritative copy is written into the App Group
    /// container (NSE-readable). When the App Group is unavailable the
    /// write falls back to the old `Application Support` location, so
    /// steady-state behaviour is unchanged on an unprovisioned build.
    public func putSnapshot(accountIdB64: String, blob: Data) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let target = writeTarget(accountIdB64: accountIdB64) else { return }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        // `combined` lays out `nonce || ciphertext || tag` per
        // RFC 5116 §3.1 — same on-disk envelope shape the Android
        // side produces (12-byte IV + ciphertext + 16-byte tag).
        guard let combined = try? AES.GCM.seal(blob, using: wrapKey).combined else { return }

        do {
            try FileManager.default.createDirectory(
                at: target.dir,
                withIntermediateDirectories: true
            )
            SharedStorage.excludeFromBackup(target.dir)
        } catch {
            // best-effort
        }
        do {
            try combined.write(to: target.url, options: .atomic)
            SharedStorage.excludeFromBackup(target.url)
        } catch {
            // best-effort
        }
    }

    /// Durably persist `blob` for `accountIdB64`, RE-THROWING on any
    /// failure — the fail-closed counterpart of
    /// ``putSnapshot(accountIdB64:blob:)``.
    ///
    /// Used on the security-critical send path (see
    /// `ChatRepository.persistSnapshotDurableOrThrow`): the MLS ratchet
    /// snapshot MUST be durably on disk BEFORE the gen-N ciphertext is
    /// published, so a write failure has to SURFACE (abort the send)
    /// rather than be swallowed. A swallowed failure here would re-open
    /// the AEAD nonce-reuse-on-crash window — see the persist-before-
    /// publish rationale in ``ChatClient/sendMessage(identity:group:plaintext:)``.
    ///
    /// The load-bearing step is the `.atomic` file write: it stages a
    /// temp file and renames it into place, so a reader never observes a
    /// torn envelope. `excludeFromBackup` failures are non-critical for
    /// durability and stay best-effort.
    public func putSnapshotDurable(accountIdB64: String, blob: Data) throws {
        let gate = try AccountStoreGate.enterOrEnd(accountIdB64)
        defer { gate.leave() }
        guard let target = writeTarget(accountIdB64: accountIdB64) else {
            throw IdentitySnapshotStoreError.unavailable(
                "no directory available to persist the MLS snapshot"
            )
        }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        let sealed = try AES.GCM.seal(blob, using: wrapKey)
        // `combined` lays out `nonce || ciphertext || tag` per RFC 5116
        // §3.1 — same envelope shape ``putSnapshot`` produces.
        guard let combined = sealed.combined else {
            throw IdentitySnapshotStoreError.sealFailed
        }
        try FileManager.default.createDirectory(
            at: target.dir,
            withIntermediateDirectories: true
        )
        SharedStorage.excludeFromBackup(target.dir)
        try combined.write(to: target.url, options: .atomic)
        SharedStorage.excludeFromBackup(target.url)
    }

    /// Read + unwrap the snapshot for `accountIdB64`. Returns
    /// `nil` on missing file, Keychain failure, or AES-GCM auth
    /// failure (e.g. wrap-key rotated since the envelope was
    /// written).
    ///
    /// ADR-0041 — prefers the App Group container copy, falls back to the
    /// old `Application Support` copy so an un-migrated snapshot still
    /// reads. The wrap key is read (not minted) — a cold read with no key
    /// simply returns `nil`.
    ///
    /// ADR-0041 LOW-1 (defence in depth) — the fallback is on **open**
    /// failure, not just presence: if the preferred (shared) copy exists
    /// but fails to AES-GCM-open (e.g. a truncated file left by an
    /// interrupted pre-atomic-migration copy) the intact old copy is
    /// opened rather than shadowed by a `nil` return. `nil` still results
    /// only when every copy is absent or unopenable (fail-closed — the
    /// irreplaceable MLS snapshot is never guessed).
    public func getSnapshot(accountIdB64: String) -> Data? {
        let candidates = readCandidates(accountIdB64: accountIdB64)
        guard !candidates.isEmpty else { return nil }
        guard let keyBytes = KeychainWrapKey.read(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount) else {
            return nil
        }
        let wrapKey = SymmetricKey(data: keyBytes)
        return SharedStorage.firstReadable(candidates) { url -> Data? in
            guard let combined = try? Data(contentsOf: url) else { return nil }
            guard let sealed = try? AES.GCM.SealedBox(combined: combined),
                  let plaintext = try? AES.GCM.open(sealed, using: wrapKey) else {
                return nil
            }
            return plaintext
        }
    }

    /// Erase the snapshot for `accountIdB64` from **both** the App Group
    /// container and the old location. No-op if absent.
    public func deleteSnapshot(accountIdB64: String) {
        if let n = newURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: n)
        }
        if let o = oldURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: o)
        }
    }

    /// ADR-0041 Phase 1 — one-time, idempotent, **copy-not-move**
    /// migration of the snapshot into the shared storage the NSE can
    /// read: the AES wrap key into the shared Keychain access group, and
    /// the envelope file into the App Group container. Never deletes the
    /// old copies.
    public func migrateToSharedStorage(accountIdB64: String) {
        // Wrap key first so the copied envelope is decryptable from the
        // shared access group.
        KeychainWrapKey.migrate(service: Self.wrapKeyService, account: Self.wrapKeyAccount)
        guard let old = oldURL(accountIdB64: accountIdB64),
              let shared = newURL(accountIdB64: accountIdB64) else { return }
        if SharedStorage.migrateFileIfNeeded(old: old, shared: shared) == shared {
            SharedStorage.excludeFromBackup(shared)
        }
    }

    // MARK: - File layout (dual-location, ADR-0041)

    private func fileName(_ accountIdB64: String) -> String {
        // Replace any `/` in the base64 account id with `_` so the
        // segment is filesystem-safe — same posture as Android.
        "\(accountIdB64.replacingOccurrences(of: "/", with: "_"))\(scope).bin"
    }

    /// New authoritative location: the App Group container. `nil` when the
    /// App Group is not provisioned for this process.
    private func newEnvelopeDir() -> URL? {
        SharedStorage.containerURL()?.appendingPathComponent(Self.subdir)
    }

    private func newURL(accountIdB64: String) -> URL? {
        newEnvelopeDir()?.appendingPathComponent(fileName(accountIdB64))
    }

    private func oldURL(accountIdB64: String) -> URL? {
        oldEnvelopeDir?.appendingPathComponent(fileName(accountIdB64))
    }

    /// Ordered read candidates (preferred first): the App Group container
    /// copy, then the old copy — each included only when it exists on
    /// disk. Feeds ``SharedStorage/firstReadable(_:open:)`` so an
    /// unopenable preferred copy falls through to the old copy (ADR-0041
    /// LOW-1) rather than shadowing it.
    private func readCandidates(accountIdB64: String) -> [URL] {
        let fm = FileManager.default
        var urls: [URL] = []
        if let n = newURL(accountIdB64: accountIdB64), fm.fileExists(atPath: n.path) {
            urls.append(n)
        }
        if let o = oldURL(accountIdB64: accountIdB64), fm.fileExists(atPath: o.path) {
            urls.append(o)
        }
        return urls
    }

    /// Prefer the App Group container as the write target (single
    /// authoritative NSE-readable copy), fall back to the old location.
    private func writeTarget(accountIdB64: String) -> (dir: URL, url: URL)? {
        if let dir = newEnvelopeDir() {
            return (dir, dir.appendingPathComponent(fileName(accountIdB64)))
        }
        if let dir = oldEnvelopeDir {
            return (dir, dir.appendingPathComponent(fileName(accountIdB64)))
        }
        return nil
    }

    private static func oldEnvelopeDir() -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        return appSupport.appendingPathComponent(subdir)
    }
}

public enum IdentitySnapshotStoreError: Error {
    case keychain(status: OSStatus, operation: String)
    /// The durable-persist location could not be resolved — there is no
    /// safe place to write the MLS snapshot, so the fail-closed send path
    /// must abort. Only ``IdentitySnapshotStore/putSnapshotDurable(accountIdB64:blob:)``
    /// raises it (the best-effort ``putSnapshot(accountIdB64:blob:)`` swallows it).
    case unavailable(String)
    /// AES-GCM produced no combined envelope (should be unreachable);
    /// surfaced so the fail-closed persist never silently drops the write.
    case sealFailed
}
