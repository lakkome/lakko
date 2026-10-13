// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 R.8 / R.7 case 1 - the facts the notification extension needs to
// stay silent where the app is silent, mirrored into the App Group.
//
// The app drops two kinds of row before anything is stored or drawn
// (`ChatRepository.silentlyFiltered`): a row from a key an accepted reset
// retired, and a founder's re-send of a message this device already holds in
// an older segment. The extension decrypts the same rows on a push wake and
// would otherwise ring or preview them. It may not write (C-59) and may not
// open the app's own stores (the pins, the DeviceGroup synced state), so the
// app writes the minimum it needs here and the extension only ever READS it:
//
//   * the keys accepted resets retired (public account keys);
//   * this account's own person key (a public key): the author of a row this
//     device sent, which is what a re-send's sender is compared with;
//   * the segment index of each successor group this device holds (a small
//     integer per group id): "is the marker's group an OLDER segment".
//
// Which conversation a group belongs to is `ConversationLineageMirror`. The
// stored messages themselves are read from the message log, which the
// extension already opens for its fetch cursor.
//
// Foundation only (compiled into both targets). Nothing secret: public keys,
// group ids the DS already knows, and their order. One row per account,
// removed with the account's data.
public enum ContinuityMirror {

    private static let keyPrefix = "me.lakko.continuity-mirror.v1."

    /// What is mirrored for one account.
    public struct Row: Codable, Equatable {
        /// Account keys (standard base64) accepted resets retired.
        public var supersededKeysB64: [String]
        /// This account's own person key, standard base64.
        public var ownPersonKeyB64: String?
        /// Lowercase hex group id -> its segment index in its conversation,
        /// for the groups that continue another one (a root is 0 and is not
        /// stored).
        public var segmentIndexOfGroupHex: [String: Int]

        public init(
            supersededKeysB64: [String] = [], ownPersonKeyB64: String? = nil,
            segmentIndexOfGroupHex: [String: Int] = [:]
        ) {
            self.supersededKeysB64 = supersededKeysB64.sorted()
            self.ownPersonKeyB64 = ownPersonKeyB64
            self.segmentIndexOfGroupHex = segmentIndexOfGroupHex
        }

        /// Nothing mirrored: nothing is dropped and no marker matches.
        public static let empty = Row()
    }

    private static func key(_ accountIdB64: String) -> String {
        keyPrefix + accountIdB64
    }

    /// The account's row. A missing mirror, an unprovisioned suite and an
    /// unreadable blob all read as ``Row/empty``: the extension then shows
    /// what it would have shown without this file.
    public static func read(
        accountIdB64: String?, defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> Row {
        guard let defaults, let accountIdB64,
              let raw = defaults.data(forKey: key(accountIdB64)),
              let row = try? JSONDecoder().decode(Row.self, from: raw)
        else { return .empty }
        return row
    }

    /// Replace the account's row. A row equal to the stored one is not
    /// rewritten.
    public static func replace(
        accountIdB64: String?, row: Row, defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        guard let defaults, let accountIdB64 else { return }
        if read(accountIdB64: accountIdB64, defaults: defaults) == row,
           defaults.object(forKey: key(accountIdB64)) != nil { return }
        guard let raw = try? JSONEncoder().encode(row) else { return }
        defaults.set(raw, forKey: key(accountIdB64))
    }

    /// Whether `senderKeyB64` is a key an accepted reset retired.
    public static func isSuperseded(_ senderKeyB64: String?, in row: Row) -> Bool {
        guard let senderKeyB64, !senderKeyB64.isEmpty else { return false }
        return row.supersededKeysB64.contains(senderKeyB64)
    }

    /// Remove the account's row.
    public static func clear(accountIdB64: String, defaults: UserDefaults? = SharedStorage.sharedDefaults()) {
        defaults?.removeObject(forKey: key(accountIdB64))
    }
}

/// The mark the notification extension puts on a notification it could not
/// withhold but the app shows nothing for (`NsePushOutcome.quiet`): the app
/// removes every delivered notification carrying it when it next runs.
public enum LakkoQuietNotification {
    public static let userInfoKey = "lakko_quiet"
}
