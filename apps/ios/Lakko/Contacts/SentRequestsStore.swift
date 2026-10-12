// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0066 section 3.5a rule 5 — the device's unanswered OUTGOING contact
// requests, kept across a process death.
//
// After a rotation of the account keys each unanswered request is sent again
// from the new key (its reply grant named the old mailbox). The re-send needs
// the recipient and the intro the person wrote; before this store the sent
// list lived in memory only, so a relaunch showed no pending request and a
// re-send carried no intro.
//
// A row holds the recipient (account id, the name typed), the DS message id
// and time of the last send, and the intro text. The intro is the person's
// own words, so the list is kept in the Keychain
// (`AfterFirstUnlockThisDeviceOnly`, the app's own group), not in
// `UserDefaults`. A row is removed when the request is answered, when it can
// no longer be sent, and by the account wipe.

public protocol SentRequestsStoring {
    /// The stored rows, oldest first. Empty when none (or unreadable).
    func load(accountIdB64: String) -> [SentContactRequest]
    /// Replace the account's rows; an empty list removes the item.
    func save(_ sent: [SentContactRequest], accountIdB64: String) throws
    func clear(accountIdB64: String)
}

/// The production store: one generic-password Keychain item per account.
public struct KeychainSentRequestsStore: SentRequestsStoring {

    public static let serviceTag = "me.lakko.contacts.sent-requests.v1"

    public init() {}

    public func load(accountIdB64: String) -> [SentContactRequest] {
        guard let data = KeychainJSONItem.read(service: Self.serviceTag, account: accountIdB64) else {
            return []
        }
        return SentRequests.decode(data)
    }

    public func save(_ sent: [SentContactRequest], accountIdB64: String) throws {
        guard !sent.isEmpty else {
            clear(accountIdB64: accountIdB64)
            return
        }
        try KeychainJSONItem.write(
            SentRequests.encode(sent), service: Self.serviceTag, account: accountIdB64)
    }

    public func clear(accountIdB64: String) {
        KeychainJSONItem.delete(service: Self.serviceTag, account: accountIdB64)
    }
}

/// The stored form and the pure rules of the sent list.
public enum SentRequests {

    /// The longest intro kept, in UTF-8 bytes (the composer's own bound is
    /// lower; this only keeps a stored row small). Chosen.
    public static let maxIntroBytes = 2048
    /// At most this many rows are kept; the oldest go first. Chosen.
    public static let maxRows = 256

    public static func encode(_ sent: [SentContactRequest]) throws -> Data {
        try JSONEncoder().encode(Array(sent.suffix(maxRows)))
    }

    /// Undecodable bytes read as no rows (the pre-store behaviour), never a
    /// crash.
    public static func decode(_ data: Data) -> [SentContactRequest] {
        (try? JSONDecoder().decode([SentContactRequest].self, from: data)) ?? []
    }

    /// The intro to keep with a row: the text as typed, cut at a character
    /// boundary to ``maxIntroBytes``.
    public static func storedIntro(_ intro: String) -> String {
        guard intro.utf8.count > maxIntroBytes else { return intro }
        var out = ""
        var bytes = 0
        for character in intro {
            let size = String(character).utf8.count
            if bytes + size > maxIntroBytes { break }
            out.append(character)
            bytes += size
        }
        return out
    }

    /// The intro a re-send to `recipientAccountIdB64` carries: the one stored
    /// with the newest row for that person, or none.
    public static func intro(for recipientAccountIdB64: String, in sent: [SentContactRequest]) -> String {
        sent.last { $0.recipientAccountIdB64 == recipientAccountIdB64 }?.intro ?? ""
    }

    /// The list after a new request to a person: one row per person, the new
    /// one last.
    public static func adding(_ request: SentContactRequest, to sent: [SentContactRequest]) -> [SentContactRequest] {
        sent.filter { $0.recipientAccountIdB64 != request.recipientAccountIdB64 } + [request]
    }

    /// The list after the person holding `recipientAccountIdB64` answered.
    public static func answered(
        recipientAccountIdB64: String, in sent: [SentContactRequest]
    ) -> [SentContactRequest] {
        sent.filter { $0.recipientAccountIdB64 != recipientAccountIdB64 }
    }
}
