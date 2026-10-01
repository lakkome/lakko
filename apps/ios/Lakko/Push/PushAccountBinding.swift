// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which account this device's push pseudonym was registered for.
///
/// ADR-0042, "A wake is shown only for the signed-in account (amended
/// 2026-09-30)". A device has at most one active account, and a background
/// wake is handled only for it. Every wake carries `d`, the device pseudonym
/// that subscribed (``DevicePushId``), which is minted at each sign-in and
/// cleared at Log out, so it names the sign-in session. This record pairs that
/// pseudonym with the account it was registered for, where the two background
/// paths can read it: the notification service extension (a separate process,
/// hence the App Group suite) and the VoIP handler in the app.
///
/// ## The rule
///
/// A wake is handled only when a binding exists and the wake's `d` equals the
/// bound pseudonym (compared as normalised lowercase hex). The account used is
/// then the BOUND account, never the account a reverse-map row or a
/// known-groups scan would name. Everything else is refused: the extension
/// shows its generic line with no content, and the VoIP handler reports and
/// immediately ends a nameless placeholder call.
///
/// ## Lifetime
///
/// Written when the push pseudonym is registered for the signed-in account
/// (``PushSubscriber/subscribe(accountIdB64:stillSignedIn:)``). Log out and
/// Forget device clear it FIRST, before anything else they do, so a wake that
/// lands while the rest of sign-out is still running already finds nothing to
/// decrypt as.
///
/// Foundation-only, so it compiles into the notification service extension as
/// a sibling source (see `project.yml`).
public struct PushAccountBinding: Equatable {
    /// 64 lowercase hex characters of the 32-byte device pseudonym.
    public let devicePseudonymHex: String
    /// The account key every per-account store uses (standard base64).
    public let accountIdB64: String

    public init(devicePseudonymHex: String, accountIdB64: String) {
        self.devicePseudonymHex = devicePseudonymHex
        self.accountIdB64 = accountIdB64
    }

    /// The account a wake may be handled for, or `nil` when it may not.
    ///
    /// `nil` when there is no binding, when the wake carries no `d` or a
    /// malformed one, or when its `d` differs from the bound pseudonym.
    public static func boundAccount(
        wakeDevicePseudonymHex: String?,
        binding: PushAccountBinding?
    ) -> String? {
        guard let binding, !binding.accountIdB64.isEmpty,
              let bound = normalizedPseudonymHex(binding.devicePseudonymHex),
              let wake = wakeDevicePseudonymHex.flatMap(normalizedPseudonymHex),
              wake == bound else {
            return nil
        }
        return binding.accountIdB64
    }

    /// Trim + lowercase, then require exactly 64 hex characters. `nil`
    /// otherwise, so a malformed `d` can never match anything.
    public static func normalizedPseudonymHex(_ hex: String) -> String? {
        let lowered = hex.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard lowered.count == 64,
              lowered.unicodeScalars.allSatisfy({ ("0"..."9").contains($0) || ("a"..."f").contains($0) })
        else {
            return nil
        }
        return lowered
    }

    /// Lowercase hex of raw pseudonym bytes.
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

    /// Storage key: `[devicePseudonymHex, accountIdB64]`.
    static let key = "me.lakko.push-account-binding.v1"

    private let defaults: UserDefaults?

    public init(defaults: UserDefaults?) {
        self.defaults = defaults
    }

    /// The store over the App Group suite, as the app and the extension use it.
    public static var live: PushAccountBindingStore {
        PushAccountBindingStore(defaults: SharedStorage.sharedDefaults())
    }

    /// Record that `devicePseudonym` was registered for `accountIdB64`.
    public func bind(devicePseudonym: Data, accountIdB64: String) {
        guard devicePseudonym.count == 32, !accountIdB64.isEmpty else { return }
        defaults?.set(
            [PushAccountBinding.hex(devicePseudonym), accountIdB64],
            forKey: Self.key
        )
    }

    /// The current binding, or `nil` when none is recorded.
    public func current() -> PushAccountBinding? {
        guard let pair = defaults?.stringArray(forKey: Self.key), pair.count == 2 else {
            return nil
        }
        return PushAccountBinding(devicePseudonymHex: pair[0], accountIdB64: pair[1])
    }

    /// Drop the binding. Log out and Forget device call this first.
    public func clear() {
        defaults?.removeObject(forKey: Self.key)
    }
}
