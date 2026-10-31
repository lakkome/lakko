// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0074 section 3.5a rule 5 — the device's unanswered OUTGOING contact
// requests, kept across a process death.
//
// After a rotation of the account keys each unanswered request is sent again
// from the new key (its reply grant named the old mailbox). The re-send needs
// the recipient and the intro the person wrote; before this store the sent
// list lived in memory only, so a relaunch showed no pending request and a
// re-send carried no intro.
//
// A row holds the recipient (account id, the name typed, and the name and key
// a search found them under), the DS message id and time of the last send, and
// the intro text. The intro is the person's
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
        let gate = try AccountStoreGate.enterUnlessWipedOrEnd(accountIdB64)
        defer { gate.leave() }
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

    /// Whether the list holds a request to this person, by account key or
    /// account id (their key may have rotated since it went out).
    public static func holds(
        _ sent: [SentContactRequest], accountKey: Data, accountId: Data?
    ) -> Bool {
        let keyB64 = base64Encode(accountKey)
        let idB64 = accountId.map { base64Encode($0) }
        return sent.contains { row in
            row.recipientKeyB64 == keyB64 || row.recipientAccountIdB64 == idB64
        }
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

    /// The row of the person holding `recipientAccountIdB64`, if any.
    public static func row(
        recipientAccountIdB64: String, in sent: [SentContactRequest]
    ) -> SentContactRequest? {
        sent.last { $0.recipientAccountIdB64 == recipientAccountIdB64 }
    }

    /// What `row` found its person under, as a claim for the key that answered
    /// it (`docs/ux/who-can-find-me.md` section 6.1, "the name a person was
    /// found under is theirs until their own profile arrives"), at stamp 0 so
    /// any profile of theirs replaces it.
    ///
    /// `nil` unless that key is exactly the one the row found: an answer names
    /// its account id itself, so a row matched by account id says nothing
    /// about who signed, and a found name must not go to them. A person who
    /// rotated between the search and the answer waits for their profile.
    /// `nil` too for a row with no name, and for a fingerprint, which names
    /// nobody. A name that is only `@username` is the username, not a name.
    public static func foundClaim(_ row: SentContactRequest, signatureKey: Data) -> ProfileClaim? {
        guard let name = row.recipientName?.trimmingCharacters(in: .whitespacesAndNewlines),
              !name.isEmpty,
              row.recipientKeyB64.flatMap(base64Decode) == signatureKey,
              name != displayFingerprint(signatureKey) else { return nil }
        let username = row.recipientUsername.isEmpty ? nil : row.recipientUsername
        return ProfileClaim(
            displayName: name == username.map { "@\($0)" } ? nil : name,
            username: username,
            updatedAtMs: 0)
    }

    /// The list after a Welcome made a one-to-one with `peer`: without the row
    /// of the person holding that key, under any of their keys, and that
    /// row's account id. `keyOfAccount` is the key this device pinned for an
    /// account it wrote to, `current` the person's current key.
    public static func answered(
        byPeer peer: Data, in sent: [SentContactRequest],
        keyOfAccount: (String) -> Data?, current: (Data) -> Data
    ) -> (remaining: [SentContactRequest], accountIdB64: String?) {
        let person = current(peer)
        func matches(_ row: SentContactRequest) -> Bool {
            keyOfAccount(row.recipientAccountIdB64).map(current) == person
        }
        return (sent.filter { !matches($0) }, sent.first(where: matches)?.recipientAccountIdB64)
    }

    /// The list after the person holding `recipientAccountIdB64` answered.
    public static func answered(
        recipientAccountIdB64: String, in sent: [SentContactRequest]
    ) -> [SentContactRequest] {
        sent.filter { $0.recipientAccountIdB64 != recipientAccountIdB64 }
    }
}
