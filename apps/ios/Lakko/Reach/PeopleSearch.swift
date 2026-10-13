// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// How this account may reach a person, strongest first (web `PersonReach`).
public enum PersonReach: Int, Comparable {
    /// They do not let this account find them: shown by name only.
    case none = 0
    /// They handed this account a reach key (a group, a channel, an
    /// introduction's contacts-of-contacts key).
    case grant = 1
    /// They are in Everyone: their mailbox is open.
    case open = 2
    /// Already a contact: nothing to add.
    case contact = 3

    public static func < (lhs: PersonReach, rhs: PersonReach) -> Bool { lhs.rawValue < rhs.rawValue }
}

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
    /// Shown only when it verified (a certificate, or a public lookup of it),
    /// and never for a person this account may not find.
    public let username: String?
    /// The reach seed that authorises a request to them; `nil` for a person in
    /// Everyone, whose mailbox is open.
    public let writeSeed: Data?
    public let reach: PersonReach

    public var id: Data { accountKey }

    /// Whether the row offers **Add contact** (web `canAddContact`).
    public var canAddContact: Bool {
        (reach == .open || reach == .grant) && accountId != nil
    }

    public init(
        accountKey: Data, accountId: Data?, displayName: String?, username: String?, writeSeed: Data?,
        reach: PersonReach? = nil
    ) {
        let resolved = reach ?? (writeSeed == nil ? .open : .grant)
        self.accountKey = accountKey
        self.accountId = accountId
        self.displayName = displayName
        self.username = PeopleSearch.shownUsername(username, reach: resolved)
        self.writeSeed = resolved == .grant ? writeSeed : nil
        self.reach = resolved
    }
}

/// What this device knows about a person, by account key (web `KnownPerson`).
public struct KnownPerson: Equatable {
    /// A name this device holds (a rename or a profile), `nil` for none.
    public var name: String?
    /// The name of last resort: a contact's fingerprint when no name is held.
    /// A contact is always named, but a name the person published (their
    /// public record, an introduction) beats this one, so a name search finds
    /// them by the name they chose rather than dropping a row that reads as a
    /// fingerprint. `nil` for anyone who is not a contact.
    public var fallbackName: String?
    public var verifiedUsername: String?
    public var isContact: Bool
    /// The best grant seed kept for them, `nil` for none.
    public var keptSeed: Data?

    public init(
        name: String?, fallbackName: String? = nil, verifiedUsername: String?, isContact: Bool,
        keptSeed: Data?
    ) {
        self.name = name
        self.fallbackName = fallbackName
        self.verifiedUsername = verifiedUsername
        self.isContact = isContact
        self.keptSeed = keptSeed
    }

    /// The name a local listing shows: the held name, else the fallback.
    public var shownName: String? { name ?? fallbackName }

    public static let unknown = KnownPerson(name: nil, verifiedUsername: nil, isContact: false, keptSeed: nil)
}

/// A person this device already knows: a contact, a member of one of its
/// groups, or a member of one of its channels whose profile it holds (web
/// `LocalCandidate`).
public struct LocalCandidate: Equatable {
    public var accountKey: Data
    public var accountId: Data?
    public var known: KnownPerson

    public init(accountKey: Data, accountId: Data?, known: KnownPerson) {
        self.accountKey = accountKey
        self.accountId = accountId
        self.known = known
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
///
/// ADR-0060 section 5: every OPRF round yields an `OprfResult`. Its
/// `lookupKey` is the only half a port sends to discovery (and the value a
/// username certificate binds); its `output` never leaves the device and is
/// what public and introduction records are opened with.
public struct PeopleSearchPorts {
    /// `/discovery/lookup` by a username's LOOKUP KEY -> the account and its
    /// public record, or `nil` (only people in Everyone answer).
    public var lookupUsername: (Data) async throws -> PublicAccountHit?
    /// One OPRF round over a username.
    public var usernameOprf: (String) async throws -> OprfResult
    /// One OPRF round over a name term.
    public var nameTermOprf: (String) async throws -> OprfResult
    /// `/discovery/name/lookup` by a name term's LOOKUP KEY -> the people in
    /// Everyone that published it, each with its public record.
    public var nameLookup: (Data) async throws -> [PublicAccountHit]
    /// account id -> the identity (account) public key, verified.
    public var identityKey: (Data) async throws -> Data?
    /// Is the key of `seed` still registered on the mailbox of `accountKey`?
    public var reachCheck: (Data, Data) async throws -> Bool
    /// Verify a username certificate for `(certificate, lookupKey, accountId)`
    /// — the certificate binds the LOOKUP KEY, never the output.
    public var verifyCertificate: (Data, Data, Data) async -> Bool
    /// ADR-0074 section 3.5a — the current account key of the person a key
    /// belongs to, across PROVEN rotations only (the key itself otherwise).
    /// Read after ``identityKey`` ran, so a rotation that fetch proved is
    /// already in the map. Defaults to "no rotation known".
    public var currentKey: (Data) async -> Data = { $0 }

    public init(
        lookupUsername: @escaping (Data) async throws -> PublicAccountHit?,
        usernameOprf: @escaping (String) async throws -> OprfResult,
        nameTermOprf: @escaping (String) async throws -> OprfResult,
        nameLookup: @escaping (Data) async throws -> [PublicAccountHit],
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

    /// These ports with the proven-predecessor resolver given.
    public func with(currentKey: @escaping (Data) async -> Data) -> PeopleSearchPorts {
        var next = self
        next.currentKey = currentKey
        return next
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

    /// At most this many usernames of introduction results are verified per
    /// search beyond the typed one: each verification costs an OPRF round,
    /// i.e. one anonymous token (web `MAX_EXTRA_USERNAME_CHECKS`). The rest
    /// show their name without a username.
    public static let maxExtraUsernameChecks = 3

    /// Normalise for local matching: letter case and accents never matter,
    /// whitespace collapsed (web `fallbackNormalize`).
    static func normalizeForMatch(_ input: String) -> String? {
        let folded = input.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: nil)
            .lowercased()
        let words = folded.split(whereSeparator: { $0.isWhitespace }).map(String.init)
        return words.isEmpty ? nil : words.joined(separator: " ")
    }

    /// The local match (web `matchesLoosely`): every word of the query is a
    /// part of some word of the name. `ann`, `nna` and `weber an` all find
    /// `Anna Weber`.
    public static func localMatches(_ name: String?, query: String) -> Bool {
        guard let name, let q = normalizeForMatch(query), let n = normalizeForMatch(name) else { return false }
        let words = n.split(separator: " ")
        return q.split(separator: " ").allSatisfy { part in words.contains { $0.contains(part) } }
    }

    /// Whether a verified username may be shown (UX section 6.2): a username is
    /// a handle for finding someone, so it goes with the permission to use it.
    public static func shownUsername(_ verified: String?, reach: PersonReach) -> String? {
        reach == .none ? nil : verified
    }

    /// A local candidate as a result, with its reach worked out.
    static func localResult(_ c: LocalCandidate, name: String) -> FoundPerson {
        let reach: PersonReach = c.known.isContact ? .contact : (c.known.keptSeed != nil ? .grant : .none)
        return FoundPerson(
            accountKey: c.accountKey, accountId: c.accountId, displayName: name,
            username: c.known.verifiedUsername, writeSeed: c.known.keptSeed, reach: reach)
    }

    /// The local half (web `localPeople`): the people this device already
    /// knows — contacts and group / channel members, with or without a grant —
    /// whose name, or whose verified username for someone this account may
    /// find, matches the query loosely. Someone without a grant is listed by
    /// name only (reach `none`, no Add contact); a candidate with no name is
    /// never listed.
    public static func local(
        query: String, candidates: [LocalCandidate], ownAccountKey: Data?
    ) -> [FoundPerson] {
        guard normalizeForMatch(query) != nil else { return [] }
        let bare = NameLookup.bareName(query)
        var seen = Set<Data>()
        var out: [FoundPerson] = []
        for c in candidates {
            guard c.accountKey != ownAccountKey, !seen.contains(c.accountKey),
                  let name = c.known.shownName else { continue }
            let result = localResult(c, name: name)
            let byName = localMatches(name, query: query)
            let byUsername = result.username.map { !bare.isEmpty && localMatches($0, query: bare) } ?? false
            guard byName || byUsername else { continue }
            seen.insert(c.accountKey)
            out.append(result)
        }
        return out
    }

    /// Merge result lists into one keyed on the account key (web
    /// `mergePeople`): the strongest reach wins, the first list's name is kept
    /// (the local one, which respects a rename), a username is kept from
    /// whichever list verified one and gated again by the merged reach. The
    /// own account never appears.
    public static func merge(_ lists: [[FoundPerson]], ownAccountKey: Data?) -> [FoundPerson] {
        var byKey: [Data: FoundPerson] = [:]
        var order: [Data] = []
        for list in lists {
            for p in list where p.accountKey != ownAccountKey {
                guard let held = byKey[p.accountKey] else {
                    byKey[p.accountKey] = p
                    order.append(p.accountKey)
                    continue
                }
                let stronger = p.reach > held.reach ? p : held
                let reach = stronger.reach
                byKey[p.accountKey] = FoundPerson(
                    accountKey: held.accountKey, accountId: held.accountId ?? p.accountId,
                    displayName: held.displayName, username: held.username ?? p.username,
                    writeSeed: reach == .grant ? (stronger.writeSeed ?? held.writeSeed ?? p.writeSeed) : nil,
                    reach: reach)
            }
        }
        return order.compactMap { byKey[$0] }
    }

    /// One introduction record, opened with the OPRF output of the username or
    /// of the name term and filtered by the whole query: by `nameMatchesQuery`
    /// when opened under a name term, by the exact username when opened under
    /// the username's output.
    static func openRecord(
        _ record: Data,
        nameOprf: OprfResult?,
        usernameOprf: (oprf: OprfResult, canonical: String)?,
        query: String
    ) -> (payload: FfiIntroductionPayload, byUsername: Bool)? {
        guard let tags = try? introductionTags(record: record) else { return nil }
        if let u = usernameOprf, let tag = try? fofTag(oprfOutput: u.oprf.output), tags.contains(tag),
           let payload = try? openIntroduction(record: record, oprfOutput: u.oprf.output),
           payload.username == u.canonical {
            return (payload, true)
        }
        if let n = nameOprf, let tag = try? fofTag(oprfOutput: n.output), tags.contains(tag),
           let payload = try? openIntroduction(record: record, oprfOutput: n.output),
           let name = payload.displayName,
           nameMatchesQuery(displayName: name, query: query) {
            return (payload, false)
        }
        return nil
    }

    /// A result built from what is known locally, over a network finding
    /// (web `resultFor`): a contact stays a contact, a name this device holds
    /// wins over the found one, the found one wins over a contact's
    /// fingerprint, and a person with no name at all is dropped.
    static func result(
        key: Data, accountId: Data?, known: KnownPerson, foundReach: PersonReach,
        foundName: String?, foundUsername: String?, seed: Data?
    ) -> FoundPerson? {
        let reach: PersonReach = known.isContact ? .contact : foundReach
        guard let name = known.name ?? foundName ?? known.fallbackName else { return nil }
        return FoundPerson(
            accountKey: key, accountId: accountId, displayName: name,
            username: foundUsername ?? known.verifiedUsername, writeSeed: seed, reach: reach)
    }

    /// The network half (web `searchPeople`). `records` are the introduction
    /// records this device holds (relayed bundles and its contacts' own
    /// offers); `known` answers what this device knows about an account key.
    public static func network(
        query: String,
        records: [Data],
        known: (Data) -> KnownPerson = { _ in .unknown },
        ownAccountKey: Data? = nil,
        ports: PeopleSearchPorts
    ) async -> PeopleSearchOutcome {
        let bare = NameLookup.bareName(query)
        var failed = false
        var allowance = false
        func note(_ error: Error) {
            if AnonTokenUnavailable.isQuotaExhausted(error) { allowance = true } else { failed = true }
        }

        // 1. The username, when the text could be one (people in Everyone).
        var byUsername: [FoundPerson] = []
        var usernameOprf: (oprf: OprfResult, canonical: String)?
        if NameLookup.looksLikeAName(bare) {
            let canonical = normalizeUsername(input: bare)
            do {
                let oprf = try await ports.usernameOprf(canonical)
                usernameOprf = (oprf, canonical)
                // The key comes from the identity bundle discovery serves for
                // the account, never from a profile's claimed account id.
                if let hit = try await ports.lookupUsername(oprf.lookupKey),
                   let key = try await ports.identityKey(hit.accountId), key.count == 32 {
                    // The public record's name counts only when it names the
                    // account and the key the verified identity bundle serves.
                    let published = await Self.openPublic(
                        hit.publicRecord, output: oprf.output, accountId: hit.accountId,
                        identityKey: key, ports: ports)?.displayName
                    // Discovery itself resolved this username to this account:
                    // that is the binding a certificate would prove.
                    if let r = result(
                        key: key, accountId: hit.accountId, known: known(key), foundReach: .open,
                        foundName: published ?? "@\(canonical)", foundUsername: canonical, seed: nil) {
                        byUsername.append(r)
                    }
                }
            } catch {
                note(error)
            }
        }

        // 2. One name term (the query's longest word), in Everyone.
        var byName: [FoundPerson] = []
        var nameOprf: OprfResult?
        if let term = nameQueryTerm(query: query) {
            do {
                let oprf = try await ports.nameTermOprf(term)
                nameOprf = oprf
                for hit in try await ports.nameLookup(oprf.lookupKey) {
                    // The person is the key the verified identity bundle
                    // serves for the hit's account; a hit without one is
                    // dropped. Named from a name this device holds for that
                    // key, else from the public record, opened with the term's
                    // OPRF output and accepted only when it names that same
                    // account AND that same key.
                    guard let key = try await ports.identityKey(hit.accountId), key.count == 32 else {
                        continue
                    }
                    let published = await Self.openPublic(
                        hit.publicRecord, output: oprf.output, accountId: hit.accountId,
                        identityKey: key, ports: ports)?.displayName
                    let k = known(key)
                    guard let name = k.name ?? published ?? k.fallbackName,
                          nameMatchesQuery(displayName: name, query: query),
                          let r = result(
                              key: key, accountId: hit.accountId, known: k, foundReach: .open,
                              foundName: published, foundUsername: nil, seed: nil) else { continue }
                    byName.append(r)
                }
            } catch {
                note(error)
            }
        }

        // 3. Contacts of contacts: the introduction records, matched locally.
        var byIntroduction: [FoundPerson] = []
        var extraChecks = 0
        for record in records {
            // An introduction record always carries the `fof` grant that
            // reaches its owner; one without it gives this device no way in.
            guard let (payload, _) = openRecord(
                record, nameOprf: nameOprf, usernameOprf: usernameOprf, query: query),
                  let fof = payload.fof else { continue }
            // The record is the introduced person's own claim: its account id
            // must be the account the identity bundle publishes this key for,
            // or a record could pair its author's key with someone else's
            // account id — and that account's username certificate.
            // ADR-0074 section 3.5a — a record written before its owner
            // rotated names the key they held then: the same person when that
            // key is a PROVEN predecessor of the published one. The result is
            // then the person under their current key.
            let personKey: Data
            do {
                guard let published = try await ports.identityKey(payload.accountId) else { continue }
                let currentOfClaimed = await ports.currentKey(payload.identityPk)
                guard Self.acceptsPublishedKey(
                    found: payload.identityPk, published: published, currentOfFound: currentOfClaimed
                ) else { continue }
                personKey = published
            } catch {
                failed = true
                continue
            }
            var verified: String?
            if let claimed = payload.username, let cert = payload.usernameCertificate {
                var lookupKey: Data?
                if let u = usernameOprf, claimed == u.canonical {
                    lookupKey = u.oprf.lookupKey
                } else if extraChecks < maxExtraUsernameChecks {
                    extraChecks += 1
                    lookupKey = try? await ports.usernameOprf(claimed).lookupKey
                }
                if let lookupKey, await ports.verifyCertificate(cert, lookupKey, payload.accountId) {
                    verified = claimed
                }
            }
            let name = payload.displayName ?? verified.map { "@\($0)" }
                ?? displayFingerprint(personKey)
            if let r = result(
                key: personKey, accountId: payload.accountId, known: known(personKey),
                foundReach: .grant, foundName: name, foundUsername: verified, seed: fof.seed) {
                byIntroduction.append(r)
            }
        }

        // One list, then the reach check: a narrowed setting takes effect at
        // the next search, not at the next rotation. A check that says no
        // drops the result silently.
        var found: [FoundPerson] = []
        for p in merge([byUsername, byName, byIntroduction], ownAccountKey: ownAccountKey) {
            guard p.reach == .grant else {
                found.append(p)
                continue
            }
            guard let seed = p.writeSeed else { continue }
            do {
                if try await ports.reachCheck(p.accountKey, seed) { found.append(p) }
            } catch {
                failed = true
            }
        }
        return outcome(found: found, allowanceUsed: allowance, failed: failed)
    }

    /// A public record served beside `accountId`, opened with the OPRF
    /// `output` of the searched term; `nil` when it does not open, names
    /// another account, or carries another identity key than `identityKey`,
    /// the key the verified identity bundle serves for that account.
    ///
    /// ADR-0074 section 3.5a — `currentKey` resolves the record's key across
    /// PROVEN rotations: a record published before its owner rotated still
    /// names the same person.
    static func openPublic(
        _ record: Data, output: Data, accountId: Data, identityKey: Data,
        currentKey: (Data) -> Data = { $0 }
    ) -> FfiIntroductionPayload? {
        guard let payload = try? openPublicRecord(record: record, oprfOutput: output),
              payload.accountId == accountId,
              acceptsPublishedKey(
                found: payload.identityPk, published: identityKey,
                currentOfFound: currentKey(payload.identityPk)) else { return nil }
        return payload
    }

    /// ``openPublic(_:output:accountId:identityKey:currentKey:)`` with the
    /// record's key resolved through the ports' proven-predecessor resolver.
    static func openPublic(
        _ record: Data?, output: Data, accountId: Data, identityKey: Data, ports: PeopleSearchPorts
    ) async -> FfiIntroductionPayload? {
        guard let record, let claimed = try? openPublicRecord(record: record, oprfOutput: output) else {
            return nil
        }
        let current = await ports.currentKey(claimed.identityPk)
        return openPublic(
            record, output: output, accountId: accountId, identityKey: identityKey,
            currentKey: { _ in current })
    }

    /// ADR-0074 section 3.5a — whether a hit that names the key `found` is
    /// the person the verified identity bundle publishes `published` for: the
    /// same key, or a key whose PROVEN current key (`currentOfFound`, from the
    /// proven-predecessor resolver) is the published one. An unproven
    /// mismatch is refused: an unproven change is in no map, so its
    /// `currentOfFound` is `found` itself.
    public static func acceptsPublishedKey(found: Data, published: Data, currentOfFound: Data) -> Bool {
        found == published || currentOfFound == published
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
        case .notFound: return Localization.text("search.people.not_found")
        case .failed: return Localization.text("search.people.failed")
        case .allowanceUsed: return AnonTokenCopy.searchAllowanceUsed
        }
    }
}

/// People search wired to the running app: the address book, the group
/// rosters, the profile directory and the kept reach grants (web
/// `search/wiring.ts`). No rule of its own lives here.
@MainActor
enum PeopleSearchWiring {

    /// What this device knows about the person with account key `keyB64`
    /// (web `knownPerson`): anyone, a contact included, is named by a name
    /// this device holds; a contact with none keeps a name of last resort
    /// (``contactFallbackName(_:)``), which a published name beats.
    static func known(keyB64: String, contactKeys: Set<String>) -> KnownPerson {
        let isContact = contactKeys.contains(keyB64)
        let resolved = ProfileDirectory.shared.resolve(signatureKeyB64: keyB64)
        let named = resolved?.source == .localRename || resolved?.source == .profileName
        return KnownPerson(
            name: named ? resolved?.name : nil,
            fallbackName: isContact && !named ? resolved.map(contactFallbackName) : nil,
            verifiedUsername: resolved?.verifiedUsername,
            isContact: isContact,
            keptSeed: ReachCenter.shared.heldSeed(forAccountKeyB64: keyB64, groupIdHex: nil))
    }

    /// A contact's name of last resort: their verified username when one is
    /// known, else their fingerprint — never the neutral "Member" placeholder,
    /// which would name every such contact alike.
    static func contactFallbackName(_ resolved: ResolvedDisplayName) -> String {
        resolved.source == .verifiedUsername ? resolved.name : resolved.fingerprint
    }

    /// The people this device already knows (web `localCandidates`): its
    /// contacts, the people in its groups (the rosters' account keys) and
    /// every profile it holds (group and channel members alike).
    static func localCandidates(
        contacts: [(accountKey: Data, accountId: Data?)], groupPeopleKeys: [Data],
        profiles: [String: ProfileClaim]
    ) -> [LocalCandidate] {
        var ids: [Data: Data?] = [:]
        var order: [Data] = []
        func add(_ key: Data, _ accountId: Data?) {
            if let held = ids[key] {
                if held == nil, let accountId { ids[key] = accountId }
                return
            }
            ids[key] = accountId
            order.append(key)
        }
        for c in contacts { add(c.accountKey, c.accountId) }
        for k in groupPeopleKeys { add(k, nil) }
        for (keyB64, claim) in profiles.sorted(by: { $0.key < $1.key }) {
            guard let key = Data(base64Encoded: keyB64) else { continue }
            add(key, claim.accountId)
        }
        let contactKeys = Set(contacts.map { $0.accountKey.base64EncodedString() })
        return order.map { key in
            let keyB64 = key.base64EncodedString()
            // A claimed account id only locates the identity bundle; the send
            // path checks the bundle's key against this key before sending.
            let accountId = (ids[key] ?? nil) ?? profiles[keyB64]?.accountId
            return LocalCandidate(
                accountKey: key, accountId: accountId,
                known: known(keyB64: keyB64, contactKeys: contactKeys))
        }
    }
}
