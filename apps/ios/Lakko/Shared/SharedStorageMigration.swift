// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0041 Phase 1 — the one-time, idempotent, fail-open migration that
/// relocates the five push-decrypt inputs for the current account into
/// the shared App Group container / Keychain access group / suite so the
/// NSE process can read them.
///
/// Runs at launch (`LakkoApp` `.task`) for the rehydrated session
/// account. It is **safe to run on every launch**:
///
///   * each input's migration is **copy-if-absent** and **copy-not-move**
///     — re-running finds the shared copy already present and no-ops,
///   * the old app-private copies are **never deleted** in this phase, so
///     an interrupted migration can never strand the foreground app (its
///     reads fall back to the old location),
///   * each input migrates **independently** and defensively — a failure
///     of one MUST NOT crash the app or block the others.
///
/// The two irreplaceable inputs — the **AccountIdentity seed** (losing it
/// loses the account identity) and the **MLS snapshot** (losing it loses
/// group/message history) — are handled with the same copy-not-move,
/// never-delete-old discipline as the rest; the reads prefer the shared
/// copy and fall back to the old one, so neither can be lost by this
/// relocation.
public enum SharedStorageMigration {

    /// Migrate the five decrypt inputs for `accountIdB64`. Idempotent and
    /// resilient — every step is best-effort and swallows its own
    /// failures so the caller never has to guard it.
    public static func runIfNeeded(accountIdB64: String) {
        // 1. AccountIdentity seed (Keychain generic-password → shared
        //    access group). The load-bearing, highest-sensitivity input.
        AccountIdentitySeedStore.migrateToSharedAccessGroup(accountIdB64: accountIdB64)

        // 2. MLS group snapshot (file → App Group container, + wrap key →
        //    shared access group). Irreplaceable: losing it loses history.
        IdentitySnapshotStore().migrateToSharedStorage(accountIdB64: accountIdB64)

        // 3. Per-group cursor + cached messages (file → App Group
        //    container, + wrap key → shared access group).
        MessageLogStore().migrateToSharedStorage(accountIdB64: accountIdB64)

        // 4. Known-groups set (UserDefaults.standard → shared suite).
        KnownGroupsStore().migrateToSharedSuite(accountIdB64: accountIdB64)
    }
}
