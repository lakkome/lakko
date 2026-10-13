// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 R.5 - which CONVERSATION a successor group belongs to, mirrored
// into the App Group so the notification extension can answer it.
//
// A conversation that continued in a new MLS group keeps its row, its title,
// its notification grouping and its mute. The app knows the lineage from the
// DeviceGroup synced state, which lives in the account's encrypted sync blob;
// C-59 forbids the extension from opening that, exactly as it does the
// thread flags (`ConversationMuteMirror`). So the app writes the one fact the
// extension needs - `successor group id -> conversation id` - here, and the
// extension only ever READS it.
//
// Foundation only (compiled into both targets). Group ids, nothing else: the
// DS already knows each of them, and the pairing of two of them is what the
// lineage register is. Rows are per account and removed with the account's
// data.
public enum ConversationLineageMirror {

    private static let keyPrefix = "me.lakko.conversation-lineage.v1."

    private static func accountPrefix(_ accountIdB64: String) -> String {
        keyPrefix + accountIdB64 + "."
    }

    private static func key(_ accountIdB64: String, _ groupIdHex: String) -> String {
        accountPrefix(accountIdB64) + groupIdHex.lowercased()
    }

    /// The conversation `groupIdHex` belongs to: the mirrored root, or the
    /// group itself (a root, an unknown group, a missing mirror: every group
    /// is its own conversation).
    public static func conversationHex(
        accountIdB64: String?,
        groupIdHex: String,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> String {
        guard let defaults, let accountIdB64,
              let root = defaults.string(forKey: key(accountIdB64, groupIdHex)), !root.isEmpty
        else { return groupIdHex }
        return root
    }

    /// Replace the account's rows with `conversationOfSuccessor` (successor
    /// group hex -> conversation hex). Rows that did not change are not
    /// rewritten.
    public static func replaceAll(
        accountIdB64: String?,
        conversationOfSuccessor: [String: String],
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        guard let defaults, let accountIdB64 else { return }
        let prefix = accountPrefix(accountIdB64)
        var wanted: [String: String] = [:]
        for (successor, conversation) in conversationOfSuccessor
        where successor.lowercased() != conversation.lowercased() {
            wanted[key(accountIdB64, successor)] = conversation.lowercased()
        }
        for (storageKey, _) in defaults.dictionaryRepresentation()
        where storageKey.hasPrefix(prefix) && wanted[storageKey] == nil {
            defaults.removeObject(forKey: storageKey)
        }
        for (storageKey, conversation) in wanted where defaults.string(forKey: storageKey) != conversation {
            defaults.set(conversation, forKey: storageKey)
        }
    }

    /// Remove every row of one account.
    public static func clear(
        accountIdB64: String,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        replaceAll(accountIdB64: accountIdB64, conversationOfSuccessor: [:], defaults: defaults)
    }
}
