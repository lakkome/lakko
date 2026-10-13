// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0073 R.2 / ADR-0050 - adding a person to a group checks WHOSE devices
// the directory served.
//
// iOS mirror of `apps/web/src/contacts/add-member-key.ts`.
//
// The functions that add a person (`ChatRepository.startGroupWith`,
// `ChatRepository.addMemberToGroup`) are told an account id (what the
// directory is asked for) and the account key the caller expects that person
// to have. The directory is not trusted to say whose devices it returned:
// every KeyPackage carries a signed device binding naming an account key
// (ADR-0050), and a device whose binding names another key than the expected
// one is not that person's device. It is never added, whoever the caller is:
// the rule lives in the function that adds, so no call site can forget it.
public enum AddMemberKey {

    /// The account keys a device may be bound to and still be the expected
    /// person's.
    ///
    /// | the expected key is in a proven chain this device recorded | accepted keys                         |
    /// |-------------------------------------------------------------|---------------------------------------|
    /// | no                                                          | the expected key alone                |
    /// | yes (the current key or a proven predecessor)               | the current key and every predecessor |
    ///
    /// The second row is ADR-0074 section 3.5: after a PROVEN rotation a
    /// device that was kept stays bound to the key it was enrolled under
    /// while a device paired later binds to the new one, and both are the
    /// same person. A key an accepted RESET retired is in no proven chain
    /// (the reset forgets the chain), so it is never accepted through this
    /// row.
    public static func acceptableBoundKeys(expectedKeyB64: String, proven: ProvenPredecessors) -> Set<String> {
        guard !expectedKeyB64.isEmpty else { return [] }
        var out: Set<String> = [expectedKeyB64]
        for key in proven.allKeys(of: expectedKeyB64) where !key.isEmpty { out.insert(key) }
        return out
    }

    /// The devices that are the expected person's: those whose binding names
    /// an accepted key. `boundKeyOf` returns the account key a KeyPackage's
    /// binding names (the leaf key for an unbound Phase 1 leaf), or `nil` for
    /// a binding that does not verify or a KeyPackage that does not parse;
    /// both are refused. Order is kept.
    public static func devicesBoundTo<D>(
        _ devices: [D],
        acceptedKeysB64: Set<String>,
        boundKeyOf: (D) -> String?
    ) -> [D] {
        devices.filter { device in
            guard let bound = boundKeyOf(device) else { return false }
            return acceptedKeysB64.contains(bound)
        }
    }

    /// The account key a KeyPackage is bound to, base64: its device binding's
    /// account key, the leaf key for an unbound Phase 1 leaf, `nil` when it
    /// does not parse or the binding does not verify.
    public static func boundKeyB64(keyPackage: Data) -> String? {
        do {
            if let account = try keyPackageAccountKey(keyPackage: keyPackage) {
                return account.base64EncodedString()
            }
            return try keyPackageSignatureKey(keyPackage: keyPackage).base64EncodedString()
        } catch {
            return nil
        }
    }
}
