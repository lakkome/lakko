// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which account this device registered for push.
///
/// ADR-0042, "A wake is shown only for the signed-in account (amended
/// 2026-09-30)", with ADR-0078 section 3. A device has at most one active
/// account, and a background wake is handled only for it. There is no device
/// pseudonym any more: every wake carries `s`, the push-gw subscription id it
/// was delivered for, and the subscription ids live in the bound account's
/// route rows (``PushRoutingStore/subscriptionId(accountIdB64:pushGidHex:)``).
/// This record names the bound account, where the two background paths can
/// read it: the notification service extension (a separate process, hence
/// the App Group suite) and the VoIP handler in the app.
///
/// ## The rule
///
/// A wake is handled only when a binding exists, the wake carries a
/// well-formed `s`, and the bound account's route row for the wake's `g`
/// holds that same id. The account used is the BOUND account. Everything else
/// is refused: the extension shows its generic line with no content, and the
/// VoIP handler reports and immediately ends a nameless placeholder call.
///
/// ## Lifetime
///
/// Written when push is set up for the signed-in account
/// (``PushSubscriber/subscribe(accountIdB64:stillSignedIn:)``). Log out and
/// Forget device clear it FIRST, before anything else they do, so a wake that
/// lands while the rest of sign-out is still running already finds nothing to
/// decrypt as.
///
/// Foundation-only, so it compiles into the notification service extension as
/// a sibling source (see `project.yml`).
public struct PushAccountBinding: Equatable {
    /// The account key every per-account store uses (standard base64).
    public let accountIdB64: String

    public init(accountIdB64: String) {
        self.accountIdB64 = accountIdB64
    }

    /// The account a wake may be handled for, or `nil` when it may not.
    ///
    /// `nil` when there is no binding, or the wake carries no `s` or a
    /// malformed one. The caller must still require the bound account's route
    /// row to hold the same id (``PushRoutingStore/lookupRoute(accountIdB64:pushGidHex:subscriptionIdHex:)``).
    public static func boundAccount(
        wakeSubscriptionIdHex: String?,
        binding: PushAccountBinding?
    ) -> String? {
        guard let binding, !binding.accountIdB64.isEmpty,
              wakeSubscriptionIdHex.flatMap(normalizedSubscriptionIdHex) != nil else {
            return nil
        }
        return binding.accountIdB64
    }

    /// Trim + lowercase, then require exactly 32 hex characters (16 bytes).
    /// `nil` otherwise, so a malformed `s` can never match anything.
    public static func normalizedSubscriptionIdHex(_ hex: String) -> String? {
        let lowered = hex.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard lowered.count == 32,
              lowered.unicodeScalars.allSatisfy({ ("0"..."9").contains($0) || ("a"..."f").contains($0) })
        else {
            return nil
        }
        return lowered
    }

    /// Lowercase hex of raw bytes.
    public static func hex(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}

/// The persisted binding, in the App Group suite both background paths read.
///
/// One row, overwritten on every bind: a device has at most one active
/// account. A `nil` suite (unprovisioned App Group) stores nothing and reads
/// nothing, so every wake is refused — the fail-closed direction.
public struct PushAccountBindingStore {

    /// Storage key: the bound account id. v2 since ADR-0078 (v1 carried a
    /// device pseudonym beside it).
    static let key = "me.lakko.push-account-binding.v2"

    private let defaults: UserDefaults?

    public init(defaults: UserDefaults?) {
        self.defaults = defaults
    }

    /// The store over the App Group suite, as the app and the extension use it.
    public static var live: PushAccountBindingStore {
        PushAccountBindingStore(defaults: SharedStorage.sharedDefaults())
    }

    /// Record that push is set up for `accountIdB64`.
    public func bind(accountIdB64: String) {
        guard !accountIdB64.isEmpty else { return }
        defaults?.set(accountIdB64, forKey: Self.key)
    }

    /// The current binding, or `nil` when none is recorded.
    public func current() -> PushAccountBinding? {
        guard let account = defaults?.string(forKey: Self.key), !account.isEmpty else {
            return nil
        }
        return PushAccountBinding(accountIdB64: account)
    }

    /// Drop the binding. Log out and Forget device call this first.
    public func clear() {
        defaults?.removeObject(forKey: Self.key)
    }
}
