// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0041 Phase 1 — shared-container constants + the pure
/// migration-decision helpers that make the five push-decrypt inputs
/// readable by the `LakkoNotificationService` (NSE) process.
///
/// Phase 1 is **pure storage relocation** — no NSE decrypt yet (that is
/// Phase 3), so the user-visible behaviour is unchanged. What this file
/// centralises is:
///
///   * the App Group identifier both the app and the NSE carry
///     (`com.apple.security.application-groups`),
///   * the Keychain access group used for the three migrated Keychain
///     items (the AccountIdentity seed + the two snapshot/cursor wrap
///     keys),
///   * the App Group container URL + shared `UserDefaults` suite the
///     file-backed and defaults-backed inputs move into,
///   * the two **pure, unit-tested** decision primitives every store's
///     one-time migration is built on: ``shouldCopy(sharedPresent:oldPresent:)``
///     (copy-not-move, copy-if-absent) and
///     ``effectiveValue(shared:old:)`` (read-new-with-fallback-to-old).
///
/// ## Fail-open, never fail-closed for the *foreground* app
///
/// Every accessor here can return `nil` when the shared container /
/// suite / access group is not provisioned for this process (an
/// unprovisioned simulator build, an entitlement that has not resolved,
/// …). Callers MUST treat that as "use the old app-private location" —
/// the migration is a relocation, not a gate, so a half-migrated or
/// un-provisioned state degrades to today's behaviour rather than
/// stranding the foreground app. (The NSE, by contrast, is read-only
/// and fails *closed* to the generic banner — Phase 3.)
public enum SharedStorage {

    /// The App Group both the main app (`apps/ios/Lakko/Lakko.entitlements`)
    /// and the NSE (`apps/ios/LakkoNotificationService/LakkoNotificationService.entitlements`)
    /// carry.
    public static let appGroupIdentifier = "group.me.lakko.dev.shared"

    /// The Keychain access group for the three migrated Keychain items
    /// (AccountIdentity seed + snapshot-wrap + cursor-wrap).
    ///
    /// On iOS an **App Group identifier is itself a valid
    /// `kSecAttrAccessGroup` value** — with **no** team-ID prefix —
    /// provided the App Groups entitlement is present on the process
    /// (it is, on both targets). Binding to the app group here means the
    /// access-group string is a **compile-time constant**: there is no
    /// fragile runtime team-ID discovery, and it is reachable from the
    /// entitlement both targets already carry.
    ///
    /// The additional team-scoped `keychain-access-groups` entitlement
    /// (`$(AppIdentifierPrefix)me.lakko.dev.shared`) is declared in
    /// `project.yml` for provisioning + to document the first-party-only
    /// sharing contract for review, but the runtime code path binds to
    /// this app-group access group — the more robust of the two
    /// provisioned mechanisms. Both are team-scoped (first-party only):
    /// a third-party app cannot join either.
    public static let keychainAccessGroup = appGroupIdentifier

    /// The App Group container URL, or `nil` when the App Group is not
    /// provisioned for this process. Callers fall back to the old
    /// app-private location on `nil` (fail-open to today's behaviour).
    public static func containerURL() -> URL? {
        FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: appGroupIdentifier
        )
    }

    /// The shared `UserDefaults` suite, or `nil` when the suite cannot
    /// be opened (same fail-open posture as ``containerURL()``).
    public static func sharedDefaults() -> UserDefaults? {
        UserDefaults(suiteName: appGroupIdentifier)
    }

    // MARK: - Pure migration-decision primitives (unit-tested)

    /// Copy-if-absent decision for the one-time, idempotent migration:
    /// copy the old-location value into the shared location **only** when
    /// the shared location is absent **and** the old location is present.
    ///
    ///   * shared-absent + old-present → `true`  (copy)
    ///   * both-present               → `false` (no-op — shared wins)
    ///   * shared-present             → `false` (no-op — already migrated)
    ///   * both-absent                → `false` (nothing to copy)
    ///
    /// This is **copy-not-move**: the caller never deletes the old copy,
    /// so an interrupted migration can never strand the foreground app.
    public static func shouldCopy(sharedPresent: Bool, oldPresent: Bool) -> Bool {
        !sharedPresent && oldPresent
    }

    /// Read-with-fallback decision: prefer the shared-location value,
    /// fall back to the old-location value.
    ///
    /// Naturally covers a **failed copy** (shared stays `nil` → old is
    /// used) and the **both-absent** case (→ `nil`), so no reader is ever
    /// stranded by a migration that did not (yet) complete.
    public static func effectiveValue<T>(shared: T?, old: T?) -> T? {
        shared ?? old
    }

    // MARK: - File migration (copy-not-move, idempotent)

    /// One-time, idempotent, **copy-not-move** file migration.
    ///
    /// Copies `old` → `shared` iff the shared file is absent and the old
    /// file is present (``shouldCopy(sharedPresent:oldPresent:)``),
    /// creating the shared parent directory as needed. The old file is
    /// **never deleted** — reads keep falling back to it until a future
    /// cleanup phase removes it.
    ///
    /// Returns the URL a reader should now use: the shared file if it
    /// exists (after any copy), else the old file if it exists, else
    /// `nil`. A copy failure is swallowed and degrades to the old file
    /// (the reader is never stranded), matching
    /// ``effectiveValue(shared:old:)``.
    @discardableResult
    public static func migrateFileIfNeeded(old: URL, shared: URL) -> URL? {
        let fm = FileManager.default
        let sharedPresent = fm.fileExists(atPath: shared.path)
        let oldPresent = fm.fileExists(atPath: old.path)

        if shouldCopy(sharedPresent: sharedPresent, oldPresent: oldPresent) {
            do {
                try fm.createDirectory(
                    at: shared.deletingLastPathComponent(),
                    withIntermediateDirectories: true
                )
                // copy-not-move: the authoritative old file stays put.
                try fm.copyItem(at: old, to: shared)
            } catch {
                // Copy failed — the old file remains authoritative; the
                // reader falls back to it. Never fatal.
                return oldPresent ? old : nil
            }
        }

        if fm.fileExists(atPath: shared.path) { return shared }
        return oldPresent ? old : nil
    }

    /// Best-effort exclusion of a shared-container URL from iCloud
    /// backup. MLS state is device-bound — the App Group copy must not
    /// ride an iCloud backup to another device any more than the old
    /// `Application Support` copy did.
    public static func excludeFromBackup(_ url: URL) {
        var url = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
    }
}
