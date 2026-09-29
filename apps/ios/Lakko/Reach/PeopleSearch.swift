// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// One person a search found (`docs/ux/who-can-find-me.md` section 6.1).
///
/// The row never says WHY the person was found; `writeSeed` is how this device
/// may reach them (a grant it holds, the `fof` key an introduction carried) and
/// is kept only for the contact request that follows, never persisted.
public struct FoundPerson: Equatable, Identifiable {
    /// The person's account (identity) key: what the mailbox is named after.
    public let accountKey: Data
    public let accountId: Data?
    public let displayName: String?
    /// Shown only when it verified (a certificate, or a public lookup of it).
    public let username: String?
    /// The reach seed that authorises a request to them; `nil` for a person in
    /// Everyone, whose mailbox is open.
    public let writeSeed: Data?

    public var id: Data { accountKey }

    public init(accountKey: Data, accountId: Data?, displayName: String?, username: String?, writeSeed: Data?) {
        self.accountKey = accountKey
        self.accountId = accountId
        self.displayName = displayName
        self.username = username
        self.writeSeed = writeSeed
    }
}

/// What a network search came back with (UX section 6.1 states).
public enum PeopleSearchOutcome: Equatable {
    case found([FoundPerson])
    /// "No one found" — the same whether nobody exists or nobody lets you find
    /// them; the searcher is never told which.
    case notFound
    /// Never "not found": the network failed.
    case failed
    /// The day's allowance of lookups is used up (ADR-0054).
    case allowanceUsed
}

/// The network lookups a search makes, injected so the rules run in a test.
public struct PeopleSearchPorts {
    /// `@username` -> account id, or `nil` (only people in Everyone answer).
    public var lookupUsername: (String) async throws -> Data?
    /// The 64-byte OPRF output of a username.
    public var usernameOprf: (String) async throws -> Data
    /// The 64-byte OPRF output of a name term.
    public var nameTermOprf: (String) async throws -> Data
    /// Account ids in Everyone that published a name term.
    public var nameLookup: (Data) async throws -> [Data]
    /// account id -> the identity (account) public key, verified.
    public var identityKey: (Data) async throws -> Data?
    /// Is the key of `seed` still registered on the mailbox of `accountKey`?
    public var reachCheck: (Data, Data) async throws -> Bool
    /// Verify a username certificate for `(oprf, accountId)`.
    public var verifyCertificate: (Data, Data, Data) async -> Bool

    public init(
        lookupUsername: @escaping (String) async throws -> Data?,
        usernameOprf: @escaping (String) async throws -> Data,
        nameTermOprf: @escaping (String) async throws -> Data,
        nameLookup: @escaping (Data) async throws -> [Data],
        identityKey: @escaping (Data) async throws -> Data?,
        reachCheck: @escaping (Data, Data) async throws -> Bool,
        verifyCertificate: @escaping (Data, Data, Data) async -> Bool
    ) {
        self.lookupUsername = lookupUsername
        self.usernameOprf = usernameOprf
        self.nameTermOprf = nameTermOprf
        self.nameLookup = nameLookup
        self.identityKey = identityKey
        self.reachCheck = reachCheck
        self.verifyCertificate = verifyCertificate
    }
}

/// ADR-0060 search: local first, then the network (username, one name term,
/// the introduction bundles), filtered by the whole query, reach-checked before
/// a result is shown.
public enum PeopleSearch {

    /// UX section 6.1 — the network row is offered from 2 characters.
    public static func offersNetworkSearch(_ query: String) -> Bool {
        query.trimmingCharacters(in: .whitespacesAndNewlines).count >= Int(nameSearchMinChars())
    }

    /// Local matching is as loose as the platform's local search: any part of
    /// any word, case- and accent-insensitive.
    public static func localMatches(_ name: String?, query: String) -> Bool {
        guard let name else { return false }
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return false }
        return name.range(of: q, options: [.caseInsensitive, .diacriticInsensitive]) != nil
    }

    /// The people this device can already reach and whose profiles it holds
    /// (contacts, members of groups who handed it a grant), matched loosely.
    /// `profiles` is `signature key b64 -> claim`; `seedFor` the grant held for
    /// a key, `nil` when none (such a person is not offered).
    public static func local(
        query: String,
        profiles: [String: ProfileClaim],
        seedFor: (String) -> Data?
    ) -> [FoundPerson] {
        var out: [FoundPerson] = []
        for (keyB64, claim) in profiles.sorted(by: { $0.key < $1.key }) {
            guard let seed = seedFor(keyB64), let key = Data(base64Encoded: keyB64),
                  localMatches(claim.displayName, query: query)
                    || (claim.username.map { $0.contains(NameLookup.bareName(query).lowercased()) } ?? false)
            else { continue }
            out.append(FoundPerson(
                accountKey: key, accountId: claim.accountId, displayName: claim.displayName,
                username: nil, writeSeed: seed))
        }
        return out
    }

    /// One introduction record, opened with any of `oprfs` and filtered by the
    /// whole query: by `nameMatchesQuery` when opened under a name term, by the
    /// exact username when opened under the username's output.
    static func openRecord(
        _ record: Data,
        nameOprf: Data?,
        usernameOprf: (oprf: Data, canonical: String)?,
        query: String
    ) -> (payload: FfiIntroductionPayload, byUsername: Bool)? {
        guard let tags = try? introductionTags(record: record) else { return nil }
        if let u = usernameOprf, let tag = try? fofTag(oprfOutput: u.oprf), tags.contains(tag),
           let payload = try? openIntroduction(record: record, oprfOutput: u.oprf),
           payload.username == u.canonical {
            return (payload, true)
        }
        if let n = nameOprf, let tag = try? fofTag(oprfOutput: n), tags.contains(tag),
           let payload = try? openIntroduction(record: record, oprfOutput: n),
           let name = payload.displayName,
           nameMatchesQuery(displayName: name, query: query) {
            return (payload, false)
        }
        return nil
    }

    /// The network half. `records` are the introduction records this device
    /// holds (relayed bundles and its contacts' own offers); `knownProfiles`
    /// names people in Everyone the device already has a profile for.
    public static func network(
        query: String,
        records: [Data],
        knownProfiles: [String: ProfileClaim],
        ports: PeopleSearchPorts
    ) async -> PeopleSearchOutcome {
        let bare = NameLookup.bareName(query)
        var found: [FoundPerson] = []
        var failed = false
        var allowance = false
        func note(_ error: Error) {
            if AnonTokenUnavailable.isQuotaExhausted(error) { allowance = true } else { failed = true }
        }

        // 1. The username, when the text could be one (people in Everyone).
        var usernameOprf: (oprf: Data, canonical: String)?
        if NameLookup.looksLikeAName(bare) {
            let canonical = normalizeUsername(input: bare)
            do {
                usernameOprf = (try await ports.usernameOprf(canonical), canonical)
                if let accountId = try await ports.lookupUsername(canonical),
                   let key = try await ports.identityKey(accountId) {
                    let claim = knownProfiles[key.base64EncodedString()]
                    found.append(FoundPerson(
                        accountKey: key, accountId: accountId,
                        displayName: claim?.displayName, username: canonical, writeSeed: nil))
                }
            } catch {
                note(error)
            }
        }

        // 2. One name term (the query's longest word), in Everyone.
        var nameOprf: Data?
        if let term = nameQueryTerm(query: query) {
            do {
                let oprf = try await ports.nameTermOprf(term)
                nameOprf = oprf
                for accountId in try await ports.nameLookup(oprf) {
                    // A public person is named from a profile this device holds;
                    // the server keeps no names, so one it has never seen cannot
                    // be filtered or shown.
                    guard let hit = knownProfiles.first(where: { $0.value.accountId == accountId }),
                          let name = hit.value.displayName,
                          nameMatchesQuery(displayName: name, query: query),
                          let key = Data(base64Encoded: hit.key) else { continue }
                    found.append(FoundPerson(
                        accountKey: key, accountId: accountId, displayName: name,
                        username: nil, writeSeed: nil))
                }
            } catch {
                note(error)
            }
        }

        // 3. Contacts of contacts: the introduction records, matched locally.
        for record in records {
            guard let (payload, byUsername) = openRecord(
                record, nameOprf: nameOprf, usernameOprf: usernameOprf, query: query) else { continue }
            var username: String?
            if byUsername, let u = usernameOprf, let cert = payload.usernameCertificate,
               await ports.verifyCertificate(cert, u.oprf, payload.accountId) {
                username = u.canonical
            }
            // The reach check: a narrowed setting takes effect at the next
            // search, not at the next rotation.
            do {
                guard try await ports.reachCheck(payload.identityPk, payload.fof.seed) else { continue }
            } catch {
                failed = true
                continue
            }
            found.append(FoundPerson(
                accountKey: payload.identityPk, accountId: payload.accountId,
                displayName: payload.displayName, username: username, writeSeed: payload.fof.seed))
        }

        var seen = Set<Data>()
        let unique = found.filter { seen.insert($0.accountKey).inserted }
        return outcome(found: unique, allowanceUsed: allowance, failed: failed)
    }

    /// Found people win; then a used-up allowance; then ANY network failure is
    /// "couldn't search", never "not found" (UX section 6.1).
    static func outcome(found: [FoundPerson], allowanceUsed: Bool, failed: Bool) -> PeopleSearchOutcome {
        if !found.isEmpty { return .found(found) }
        if allowanceUsed { return .allowanceUsed }
        if failed { return .failed }
        return .notFound
    }

    /// The copy for each outcome (UX section 6.1).
    public static func message(for outcome: PeopleSearchOutcome) -> String? {
        switch outcome {
        case .found: return nil
        case .notFound: return "No one found. Check the spelling, or ask them for an invite link."
        case .failed: return "Couldn't search. Check your connection and try again."
        case .allowanceUsed: return AnonTokenCopy.searchAllowanceUsed
        }
    }
}
