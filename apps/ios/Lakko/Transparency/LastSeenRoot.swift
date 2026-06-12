// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Last-seen transparency-log root store (KT-log S3d, no-rollback gate).
///
/// Mirror of `apps/web/src/transparency/last-seen-root.ts` (S3c).
///
/// The transparency log is GLOBAL and append-only — its `log_size`
/// only ever grows. We persist the highest `log_size` this client has
/// ever observed (one global value, not per-account) so that a root
/// whose `log_size` is SMALLER than what we already saw is caught as an
/// unambiguous rollback attack: the log appears to have shrunk, which a
/// correct append-only log can never do.
///
/// This is the gossip/consistency half of the equivocation defence — a
/// server that tries to show an old, smaller tree (to hide a leaf it
/// later appended) trips the rollback check. The store NEVER advances
/// on a shrink; it only advances on `newLogSize >= seen`.
///
/// The value lives in `UserDefaults` and is not secret (a public tree
/// size). Stored as a `UInt64`; `UserDefaults` round-trips it as an
/// `NSNumber` without precision loss. (Contrast ``PeerPinStore``, whose
/// rationale for `UserDefaults` over the Keychain — public, not secret,
/// must survive relaunch — applies identically here.)
public final class LastSeenRoot {

    /// Versioned storage key so a future schema change can land
    /// alongside v1 without collision. Mirror of the web
    /// `lakko.transparency.last-seen-root.v1`.
    private static let storageKey = "me.lakko.transparency.last-seen-root.v1"

    /// The backing store. Injectable so tests can use an isolated
    /// `UserDefaults(suiteName:)` instead of `.standard`.
    private let defaults: UserDefaults

    /// Outcome of ``checkAndAdvance(_:)``.
    public enum RootCheckResult: Equatable {
        /// `newLogSize >= seen`: the value was persisted (advanced or
        /// equal). The append-only invariant holds; proceed with the
        /// inclusion check.
        case ok
        /// `newLogSize < seen`: the log shrank — an unambiguous
        /// rollback attack. The store was NOT advanced. The caller
        /// treats this as a hard failure.
        case rollback
    }

    /// - Parameter defaults: the backing `UserDefaults`. Defaults to
    ///   `.standard`; tests inject an isolated suite.
    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Read the highest `log_size` ever seen, or `0` when none
    /// persisted. Exposed for tests + diagnostics.
    public func loadLastSeenLogSize() -> UInt64 {
        // `UserDefaults.object(forKey:)` is `nil` when never written;
        // an `NSNumber` otherwise. A non-numeric / negative blob (a
        // tampered or corrupt row) is treated as "never seen" rather
        // than mis-trusted.
        guard let number = defaults.object(forKey: Self.storageKey) as? NSNumber else {
            return 0
        }
        let v = number.int64Value
        return v >= 0 ? UInt64(v) : 0
    }

    /// Compare a freshly-fetched root's `log_size` against the
    /// persisted high-water mark, advancing it on a non-shrinking
    /// value.
    ///
    /// Semantics (identical to the web `checkAndAdvance`):
    ///   * `newLogSize >= seen` → persist `newLogSize` (a `==` is an
    ///     idempotent re-observation; a `>` is normal growth). Returns
    ///     `.ok`.
    ///   * `newLogSize < seen`  → the log shrank. **Refuse** — leave
    ///     the stored value untouched and return `.rollback`. NEVER
    ///     advance (or regress) on a shrink.
    ///
    /// - Parameter newLogSize: the `log_size` from the just-fetched
    ///   signed root.
    @discardableResult
    public func checkAndAdvance(_ newLogSize: UInt64) -> RootCheckResult {
        let seen = loadLastSeenLogSize()
        if newLogSize < seen {
            return .rollback
        }
        // Persist only when it actually moves forward to avoid a
        // redundant write on every steady-state observation.
        if newLogSize > seen {
            // Store via `Int64` — the highest realistic log size is
            // far below `Int64.max`, and `NSNumber(value: UInt64)`
            // round-trips through `int64Value` losslessly in that
            // range, keeping `loadLastSeenLogSize` symmetric.
            defaults.set(NSNumber(value: Int64(newLogSize)), forKey: Self.storageKey)
        }
        return .ok
    }

    /// Wipe the stored marker. For the "forget device" flow + tests.
    public func clear() {
        defaults.removeObject(forKey: Self.storageKey)
    }
}
