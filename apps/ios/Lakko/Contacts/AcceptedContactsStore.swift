// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// On-device persistence for the user's accepted contacts.
///
/// Closes (for the accepted list) the long-standing "No persistence yet"
/// gap noted on ``ContactsRepository`` and unblocks ADR-0021 group-call
/// roster name resolution: the SFU sends a per-call `peer_tag` derived
/// from each peer's account_id, and the client re-derives the same over
/// its contacts' account_ids to resolve a name — which requires the
/// accepted contacts (each carrying `request.fromAccountId`) to survive a
/// relaunch.
///
/// The accepted list is non-secret relationship metadata (peer pubkeys +
/// account_ids the user already chose to trust, and which the contacts UI
/// already displays) — the same data class the existing `PeerPinStore`
/// already persists. It is JSON-encoded into `UserDefaults`; it is never
/// sent to any server (invariant #1/#2 unaffected).
///
/// Mirror of the web `apps/web/src/contacts/local-store.ts` accepted-list
/// store.
enum AcceptedContactsStore {
    private static let storageKey = "lakko.contacts.accepted.v1"

    /// Load the persisted accepted contacts (empty on first launch or a
    /// decode failure — a forward-compat / corruption guard).
    static func load() -> [AcceptedContact] {
        guard let data = UserDefaults.standard.data(forKey: storageKey) else {
            return []
        }
        return (try? JSONDecoder().decode([AcceptedContact].self, from: data)) ?? []
    }

    /// Persist the accepted contacts. A best-effort write — an encode
    /// failure (impossible for the all-`Codable` model) leaves the prior
    /// value in place rather than clearing it.
    static func save(_ accepted: [AcceptedContact]) {
        guard let data = try? JSONEncoder().encode(accepted) else { return }
        UserDefaults.standard.set(data, forKey: storageKey)
    }
}
