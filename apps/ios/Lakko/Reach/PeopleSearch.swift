// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
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
    /// ADR-0078 — the VERIFIED mailbox the request goes to: the binding a
    /// public record, an introduction or a held grant carried, checked
    /// against the person's key. `nil` when none verified (no request can be
    /// sent then).
    public let mailbox: MailboxTarget?
    /// The whole grant `writeSeed` came from, when the search read one this
    /// device does not hold yet (the `fof` grant of an introduction record):
    /// what an add of this person keeps as a held grant
    /// (``ReachCenter/keepFoundGrant(_:accountKey:)``). `nil` for a grant
    /// already held and for a person in Everyone.
    public let grant: FfiReachGrant?
    public let reach: PersonReach
    /// Whether `writeSeed` passed a reach check during this search, so it is
    /// known to open the mailbox. `false` for a kept grant the check could not
    /// ask about, and for a person with no grant.
    public private(set) var reachChecked: Bool

    public var id: Data { accountKey }

    /// Whether the row offers **Add contact** (web `canAddContact`).
    public var canAddContact: Bool {
        (reach == .open || reach == .grant) && accountId != nil
    }

    public init(
        accountKey: Data, accountId: Data?, displayName: String?, username: String?, writeSeed: Data?,
        reach: PersonReach? = nil, mailbox: MailboxTarget? = nil, grant: FfiReachGrant? = nil,
        reachChecked: Bool = false
    ) {
        let resolved = reach ?? (writeSeed == nil ? .open : .grant)
        self.accountKey = accountKey
        self.accountId = accountId
        self.displayName = displayName
        self.username = PeopleSearch.shownUsername(username, reach: resolved)
        self.writeSeed = resolved == .grant ? writeSeed : nil
        self.mailbox = mailbox
        // Only the grant the seed is: a grant whose seed is not the one this
        // result writes with is not carried.
        self.grant = (resolved == .grant && grant?.seed == writeSeed) ? grant : nil
        self.reach = resolved
        self.reachChecked = resolved == .grant && reachChecked
    }

    /// This result after its grant passed a reach check.
    func passingReachCheck() -> FoundPerson {
        var next = self
        next.reachChecked = reach == .grant
        return next
    }

    /// This person written to with `kept`: the row's grant was refused and
    /// `kept` passed, or the grant the row already carries.
    func writing(with kept: KeptGrant) -> FoundPerson {
        FoundPerson(
            accountKey: accountKey, accountId: accountId, displayName: displayName,
            username: username, writeSeed: kept.seed, reach: .grant, mailbox: kept.mailbox,
            grant: grant, reachChecked: true)
    }

    /// This person after they became a contact, with the text still in the
    /// field: the row opens them, and nothing about how they were reached
    /// stays on it.
    var asContact: FoundPerson {
        FoundPerson(
            accountKey: accountKey, accountId: accountId, displayName: displayName,
            username: username, writeSeed: nil, reach: .contact, mailbox: mailbox)
    }

    /// This person with a username a certificate proved.
    func withProvenUsername(_ proven: String) -> FoundPerson {
        FoundPerson(
            accountKey: accountKey, accountId: accountId, displayName: displayName,
            username: proven, writeSeed: writeSeed, reach: reach, mailbox: mailbox,
            grant: grant, reachChecked: reachChecked)
    }

    /// This person as a row that offers nothing: the name alone, no username
    /// and no button.
    var nameOnly: FoundPerson {
        FoundPerson(
            accountKey: accountKey, accountId: accountId, displayName: displayName,
            username: nil, writeSeed: nil, reach: PersonReach.none)
    }
}

/// One grant this device keeps for a person, with the VERIFIED mailbox it
/// writes to. The seed is SECRET: whoever holds it can write to that mailbox.
public struct KeptGrant: Equatable {
    public let seed: Data
    public let mailbox: MailboxTarget

    public init(seed: Data, mailbox: MailboxTarget) {
        self.seed = seed
        self.mailbox = mailbox
    }
}

/// What the Delivery Service answered about the grants kept for one person
/// (`docs/ux/who-can-find-me.md` section 6.1, "A kept grant is checked before
/// it is offered").
public enum ReachVerdict: Equatable {
    /// This grant still reaches them.
    case live(KeptGrant)
    /// None of these seeds does.
    case dead(checkedSeeds: Set<Data>)

    /// Whether this verdict still speaks for `grants`: a grant received since
    /// it was given was not part of it, and a live grant may be gone.
    public func covers(_ grants: [KeptGrant]) -> Bool {
        switch self {
        case .live(let grant): return grants.contains(grant)
        case .dead(let seeds): return grants.allSatisfy { seeds.contains($0.seed) }
        }
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
    /// Every grant kept for them, best first: contact, each group, each
    /// channel, the found one, never a bare `member` grant
    /// (``ReachRules/searchGrants(from:)``). What a local row offers is the
    /// first one once a check said so; the others stay, and the send tries
    /// them.
    public var keptGrants: [KeptGrant]
    /// The first grant in ``keptGrants`` passed a reach check.
    public var keptGrantChecked: Bool
    /// They hold grants no verdict speaks for yet: the row shows the name
    /// alone until one arrives.
    public var awaitingReachCheck: Bool

    public init(
        name: String?, fallbackName: String? = nil, verifiedUsername: String?, isContact: Bool,
        keptGrants: [KeptGrant] = [], keptGrantChecked: Bool = false, awaitingReachCheck: Bool = false
    ) {
        self.name = name
        self.fallbackName = fallbackName
        self.verifiedUsername = verifiedUsername
        self.isContact = isContact
        self.keptGrants = keptGrants
        self.keptGrantChecked = keptGrantChecked
        self.awaitingReachCheck = awaitingReachCheck
    }

    /// The name a local listing shows: the held name, else the fallback.
    public var shownName: String? { name ?? fallbackName }

    public static let unknown = KnownPerson(name: nil, verifiedUsername: nil, isContact: false)
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

/// The people half of a network search before the fold: who was found, and
/// whether any question was refused for a used-up allowance or got no answer.
public struct PeopleNetworkLegs: Equatable {
    public var people: [FoundPerson]
    public var allowanceUsed: Bool
    public var failed: Bool

    public init(people: [FoundPerson], allowanceUsed: Bool, failed: Bool) {
        self.people = people
        self.allowanceUsed = allowanceUsed
        self.failed = failed
    }
}

/// A channel the search found by its `@handle`; its row opens the subscribe
/// page (UX section 6.1).
public struct FoundChannel: Equatable {
    public let idHex: String
    public let handle: String

    public init(idHex: String, handle: String) {
        self.idHex = idHex
        self.handle = handle
    }
}

/// UX section 6.1 — what the People part of the search field says.
public enum PeopleNetworkState: Equatable {
    case found
    /// The same whether nobody exists or nobody lets you find them.
    case notFound
    /// The network failed: never "not found". Typing again retries.
    case failed
    /// The search allowance (anonymous lookup tokens) is used up for now.
    case outOfSearches

    /// The line under the heading, `nil` for `found` (the rows say it).
    public var message: String? {
        switch self {
        case .found: return nil
        // `who-can-find-me.md` 6.1, "One list" (2026-11-04): the whole list,
        // not people alone, so a search that found a group never reads "No
        // one found" above it.
        case .notFound: return Localization.text("search.nothing_found")
        case .failed: return Localization.text("search.people.failed")
        case .outOfSearches: return Localization.text("search.people.out_of_searches")
        }
    }
}

/// One network search of the field: the people, the channel the typed handle
/// names, and whether any question went unanswered.
public struct PeopleSearchAnswer: Equatable {
    public var people: [FoundPerson]
    public var channel: FoundChannel?
    /// ADR-0087 section 5 — public groups and channels found by `@name` and
    /// by name ("Groups and channels").
    var conversations: [ConversationHandles.Found] = []
    public var allowanceUsed: Bool
    public var failed: Bool

    public init(people: [FoundPerson], channel: FoundChannel? = nil, allowanceUsed: Bool = false, failed: Bool = false) {
        self.people = people
        self.channel = channel
        self.allowanceUsed = allowanceUsed
        self.failed = failed
    }

    /// The people legs and the channel leg, as one answer.
    public init(legs: PeopleNetworkLegs, channel: FoundChannel?, channelAllowanceUsed: Bool, channelFailed: Bool) {
        self.init(
            people: legs.people, channel: channel,
            allowanceUsed: legs.allowanceUsed || channelAllowanceUsed,
            failed: legs.failed || channelFailed)
    }

    /// Only an answer to every question is remembered for the session: a
    /// failed or refused question is asked again when the text is typed again.
    public var remembered: Bool { !allowanceUsed && !failed }

    /// Anything found is shown, whatever else failed; nothing found is "not
    /// found" only when every question was answered. `includesChannels` is
    /// `false` in Contacts, which matches only people.
    public func state(includesChannels: Bool) -> PeopleNetworkState {
        if !people.isEmpty || (includesChannels && (channel != nil || !conversations.isEmpty)) { return .found }
        if allowanceUsed { return .outOfSearches }
        if failed { return .failed }
        return .notFound
    }
}

/// UX section 6.1 — a found person's row after **Add contact**.
public enum PersonRequestState: Equatable {
    case idle
    case sending
    /// "Request sent"; the button is gone.
    case sent
    /// Their mailbox refused the request: they changed who can find them.
    case refused
    /// Any other failure; the button is back.
    case failed

    /// The state a failed send leaves the row in.
    public static func after(error: Error) -> PersonRequestState {
        isRefusal(error) ? .refused : .failed
    }

    private static func isRefusal(_ error: Error) -> Bool {
        switch error as? ContactsError {
        case .requestRefused?: return true
        case .wrapped(let cause)?: return isRefusal(cause)
        default: return false
        }
    }

    /// The line on the row, `nil` when the row says nothing in words.
    public var message: String? {
        switch self {
        case .refused: return Localization.text("search.people.request_refused")
        case .failed: return Localization.text("search.people.request_failed")
        case .idle, .sending, .sent: return nil
        }
    }

    /// The line on a row that may or may not still offer **Add contact**: a
    /// refusal stays after the refusal took the button away.
    public func message(canAddContact: Bool) -> String? {
        canAddContact || self == .refused ? message : nil
    }

    /// Whether the row offers **Add contact**.
    public var offersAdd: Bool { self == .idle || self == .failed }
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
    /// Is the key of `seed` still registered on the mailbox given (ADR-0078:
    /// a verified one)?
    public var reachCheck: (MailboxTarget, Data) async throws -> Bool
    /// Verify a username certificate for `(certificate, lookupKey, accountId)`
    /// — the certificate binds the LOOKUP KEY, never the output.
    public var verifyCertificate: (Data, Data, Data) async -> Bool
    /// ADR-0074 section 3.5a — the current account key of the person a key
    /// belongs to, across PROVEN rotations only (the key itself otherwise).
    /// Read after ``identityKey`` ran, so a rotation that fetch proved is
    /// already in the map. Defaults to "no rotation known".
    public var currentKey: (Data) async -> Data = { $0 }
    /// A username's LOOKUP KEY, to prove a certificate a record carries. One
    /// OPRF round, one anonymous token, unless the key is kept
    /// (``KeptUsernameLookupKeys``). Defaults to ``usernameOprf``.
    public var usernameLookupKey: (String) async throws -> Data

    public init(
        lookupUsername: @escaping (Data) async throws -> PublicAccountHit?,
        usernameOprf: @escaping (String) async throws -> OprfResult,
        nameTermOprf: @escaping (String) async throws -> OprfResult,
        nameLookup: @escaping (Data) async throws -> [PublicAccountHit],
        identityKey: @escaping (Data) async throws -> Data?,
        reachCheck: @escaping (MailboxTarget, Data) async throws -> Bool,
        verifyCertificate: @escaping (Data, Data, Data) async -> Bool
    ) {
        self.lookupUsername = lookupUsername
        self.usernameOprf = usernameOprf
        self.nameTermOprf = nameTermOprf
        self.nameLookup = nameLookup
        self.identityKey = identityKey
        self.reachCheck = reachCheck
        self.verifyCertificate = verifyCertificate
        self.usernameLookupKey = { try await usernameOprf($0).lookupKey }
    }

    /// These ports with the proven-predecessor resolver given.
    public func with(currentKey: @escaping (Data) async -> Data) -> PeopleSearchPorts {
        var next = self
        next.currentKey = currentKey
        return next
    }

    /// These ports with the lookup keys of usernames read through `kept`.
    public func with(kept: KeptUsernameLookupKeys) -> PeopleSearchPorts {
        var next = self
        let fetch = usernameLookupKey
        next.usernameLookupKey = { username in
            if let held = await kept.key(for: username) { return held }
            let key = try await fetch(username)
            await kept.keep(key, for: username)
            return key
        }
        return next
    }
}

/// The lookup keys of usernames proved so far, oldest dropped first, so a
/// person found again costs no token. In memory only: a lookup key opens
/// nothing, and it is lost with the process.
public actor KeptUsernameLookupKeys {
    /// How many are kept. Chosen, not measured.
    public static let maxKept = 64

    private var held: [String: Data] = [:]
    private var order: [String] = []
    private let max: Int

    public init(max: Int = KeptUsernameLookupKeys.maxKept) {
        self.max = max
    }

    public func key(for username: String) -> Data? {
        held[username]
    }

    public func keep(_ key: Data, for username: String) {
        if held.updateValue(key, forKey: username) == nil { order.append(username) }
        while order.count > max {
            held[order.removeFirst()] = nil
        }
    }
}

/// ADR-0060 search: local first, then the network (username, one name term,
/// the introduction bundles), filtered by the whole query, reach-checked before
/// a result is shown.
public enum PeopleSearch {

    /// UX section 6.1 — the field asks the network only from this many
    /// characters (a leading `@` not counted). Chosen, not measured.
    public static let networkMinChars = 3

    /// UX section 6.1 — how long the person must stop typing before the field
    /// asks the network. Chosen, not measured.
    public static let networkDebounceMs: UInt64 = 500

    /// UX section 6.1 — how long a network answer is reused for its text, a
    /// not-found included. Past it the text is asked again the next time it
    /// is typed: a person may have widened who can find them. Checked when a
    /// text is asked; no timer re-asks an unchanged text. Chosen, not
    /// measured.
    public static let answerKeptSeconds: TimeInterval = 60

    /// UX section 6.1 — whether the typed text asks the network at all: from
    /// three characters (a leading `@` ignored), and only when the text could
    /// be a username (the shared `looks_like_a_name` rule) or its longest word
    /// (the shared name-term rule) has at least three letters. Anything else
    /// asks nothing.
    public static func asksNetwork(_ query: String) -> Bool {
        let bare = NameLookup.bareName(query.trimmingCharacters(in: .whitespacesAndNewlines))
        guard bare.count >= networkMinChars else { return false }
        if NameLookup.looksLikeAName(bare) { return true }
        return (nameQueryTerm(query: bare)?.count ?? 0) >= networkMinChars
    }

    /// The key a network search is remembered under for the session: the
    /// typed text without its `@`, with case, accents and repeated spaces
    /// folded (none of them changes what the network answers). `nil` for a
    /// text that asks nothing.
    public static func searchKey(_ query: String) -> String? {
        guard asksNetwork(query) else { return nil }
        let bare = NameLookup.bareName(query.trimmingCharacters(in: .whitespacesAndNewlines))
        return normalizeForMatch(bare)
    }

    /// At most this many usernames a record states are verified per search
    /// beyond the typed one, public records of a name match and introductions
    /// together: each verification costs an OPRF round, i.e. one anonymous
    /// token (web `MAX_EXTRA_USERNAME_CHECKS`). The rest show their name
    /// without a username.
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
        let kept = c.known.keptGrants.first
        let reach: PersonReach = c.known.isContact ? .contact : (kept != nil ? .grant : .none)
        return FoundPerson(
            accountKey: c.accountKey, accountId: c.accountId, displayName: name,
            username: c.known.verifiedUsername, writeSeed: kept?.seed, reach: reach,
            mailbox: kept?.mailbox, reachChecked: c.known.keptGrantChecked)
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
            out.append(c.known.awaitingReachCheck ? result.nameOnly : result)
        }
        return out
    }

    // MARK: - A kept grant is checked before it is offered (UX section 6.1)

    /// The candidates as the local match should see them: with a live
    /// verdict, the grant that passed comes first and the others stay for the
    /// send; with a dead one, no grant, so reach is `none`, the username is
    /// not shown and they match by name only; with none yet, waiting for it.
    /// `unanswered` are the people whose check could not be asked (a network
    /// failure): they stay as they are, and the send checks again. A contact
    /// is never checked.
    public static func applying(
        verdicts: [Data: ReachVerdict], unanswered: Set<Data>, to candidates: [LocalCandidate]
    ) -> [LocalCandidate] {
        candidates.map { candidate in
            var c = candidate
            guard !c.known.isContact, !c.known.keptGrants.isEmpty else { return c }
            if let verdict = verdicts[c.accountKey], verdict.covers(c.known.keptGrants) {
                switch verdict {
                case .live(let grant):
                    c.known.keptGrants = [grant] + c.known.keptGrants.filter { $0 != grant }
                    c.known.keptGrantChecked = true
                case .dead:
                    c.known.keptGrants = []
                }
            } else if !unanswered.contains(c.accountKey) {
                c.known.awaitingReachCheck = true
            }
            return c
        }
    }

    /// The candidates to ask about for `query`: those who hold grants, are
    /// not contacts and match the query with their grants in hand, so a kept
    /// grant's username counts. Whatever verdict is held: an answer speaks
    /// for one text only, since a person's reach changes while the field is
    /// open (UX section 6.1, "A row says what is true now").
    public static func needingReachCheck(
        query: String, candidates: [LocalCandidate], ownAccountKey: Data?
    ) -> [LocalCandidate] {
        let open = candidates.filter { c in
            !c.known.isContact && !c.known.keptGrants.isEmpty
        }
        let matched = Set(local(query: query, candidates: open, ownAccountKey: ownAccountKey).map(\.accountKey))
        return open.filter { matched.contains($0.accountKey) }
    }

    /// The reach check over `grants` in order, stopping at the first that
    /// passes. `nil` when a check threw: that is no verdict, a connection
    /// failure says nothing about the person.
    public static func reachVerdict(
        grants: [KeptGrant], check: (MailboxTarget, Data) async throws -> Bool
    ) async -> ReachVerdict? {
        for grant in grants {
            do {
                if try await check(grant.mailbox, grant.seed) { return .live(grant) }
            } catch {
                return nil
            }
        }
        return .dead(checkedSeeds: Set(grants.map(\.seed)))
    }

    /// The grant a send writes with: the row's when it still passes, else the
    /// first of `held` (every grant this device holds for the person, read
    /// fresh) that does, in the usual order. `nil` when every one was
    /// refused. Throws what the check throws: a connection failure is no
    /// refusal.
    public static func reachingGrant(
        row: KeptGrant?, held: [KeptGrant], check: (MailboxTarget, Data) async throws -> Bool
    ) async throws -> KeptGrant? {
        var tried = Set<Data>()
        for grant in (row.map { [$0] } ?? []) + held where tried.insert(grant.seed).inserted {
            if try await check(grant.mailbox, grant.seed) { return grant }
        }
        return nil
    }

    /// Merge result lists into one keyed on the account key (web
    /// `mergePeople`): the strongest reach wins, at equal reach the grant that
    /// passed a reach check, the first list's name is kept (the local one,
    /// which respects a rename), a username is kept from whichever list
    /// verified one and gated again by the merged reach. The own account never
    /// appears.
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
                let stronger: FoundPerson
                if p.reach == held.reach {
                    stronger = p.reachChecked && !held.reachChecked ? p : held
                } else {
                    stronger = p.reach > held.reach ? p : held
                }
                let reach = stronger.reach
                let seed = reach == .grant ? (stronger.writeSeed ?? held.writeSeed ?? p.writeSeed) : nil
                byKey[p.accountKey] = FoundPerson(
                    accountKey: held.accountKey, accountId: held.accountId ?? p.accountId,
                    displayName: held.displayName, username: held.username ?? p.username,
                    writeSeed: seed,
                    reach: reach,
                    mailbox: stronger.mailbox ?? held.mailbox ?? p.mailbox,
                    grant: [stronger.grant, held.grant, p.grant].compactMap { $0 }.first { $0.seed == seed },
                    reachChecked: stronger.reachChecked)
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
        query: String,
        onOpened: () -> Void = {}
    ) -> (payload: FfiIntroductionPayload, byUsername: Bool)? {
        guard let tags = try? introductionTags(record: record) else { return nil }
        var opened = false
        func noteOpened() {
            if !opened { onOpened() }
            opened = true
        }
        if let u = usernameOprf, let tag = try? fofTag(oprfOutput: u.oprf.output), tags.contains(tag),
           let payload = try? openIntroduction(record: record, oprfOutput: u.oprf.output) {
            noteOpened()
            if payload.username == u.canonical { return (payload, true) }
        }
        if let n = nameOprf, let tag = try? fofTag(oprfOutput: n.output), tags.contains(tag),
           let payload = try? openIntroduction(record: record, oprfOutput: n.output) {
            noteOpened()
            if let name = payload.displayName, nameMatchesQuery(displayName: name, query: query) {
                return (payload, false)
            }
        }
        return nil
    }

    /// A result built from what is known locally, over a network finding
    /// (web `resultFor`): a contact stays a contact, a name this device holds
    /// wins over the found one, the found one wins over a contact's
    /// fingerprint, and a person with no name at all is dropped.
    static func result(
        key: Data, accountId: Data?, known: KnownPerson, foundReach: PersonReach,
        foundName: String?, foundUsername: String?, seed: Data?, mailbox: MailboxTarget? = nil,
        grant: FfiReachGrant? = nil
    ) -> FoundPerson? {
        let reach: PersonReach = known.isContact ? .contact : foundReach
        guard let name = known.name ?? foundName ?? known.fallbackName else { return nil }
        return FoundPerson(
            accountKey: key, accountId: accountId, displayName: name,
            username: foundUsername ?? known.verifiedUsername, writeSeed: seed, reach: reach,
            // A grant's seed opens only the mailbox its own binding names.
            mailbox: foundReach == .grant ? mailbox : (mailbox ?? known.keptGrants.first?.mailbox),
            grant: grant)
    }

    /// ADR-0078 — the mailbox a public record or an introduction names: its
    /// binding verified against the identity key the payload carries (the
    /// core verified it when it opened; checked again where it is used).
    static func mailbox(of payload: FfiIntroductionPayload?) -> MailboxTarget? {
        guard let payload else { return nil }
        return MailboxTarget.verifiedOrNil(binding: payload.mailboxBinding, accountKey: payload.identityPk)
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
        let legs = await networkLegs(
            query: query, records: records, known: known, ownAccountKey: ownAccountKey, ports: ports)
        return outcome(found: legs.people, allowanceUsed: legs.allowanceUsed, failed: legs.failed)
    }

    /// ``network(query:records:known:ownAccountKey:ports:)`` before the fold:
    /// the people found and whether any question was refused for a used-up
    /// allowance or got no answer. The search field needs the unfolded form,
    /// because an answer with a failed question is not remembered (typing
    /// again retries it).
    public static func networkLegs(
        query: String,
        records: [Data],
        known: (Data) -> KnownPerson = { _ in .unknown },
        ownAccountKey: Data? = nil,
        ports: PeopleSearchPorts
    ) async -> PeopleNetworkLegs {
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
                    let record = await Self.openPublic(
                        hit.publicRecord, output: oprf.output, accountId: hit.accountId,
                        identityKey: key, ports: ports)
                    let published = record?.displayName
                    // Discovery itself resolved this username to this account:
                    // that is the binding a certificate would prove.
                    if let r = result(
                        key: key, accountId: hit.accountId, known: known(key), foundReach: .open,
                        foundName: published ?? "@\(canonical)", foundUsername: canonical, seed: nil,
                        mailbox: Self.mailbox(of: record)) {
                        byUsername.append(r)
                    }
                }
            } catch {
                note(error)
            }
        }

        // 2. One name term (the query's longest word), in Everyone.
        var namedOnly: [FoundPerson] = []
        var nameClaims: [Data: (username: String, certificate: Data, accountId: Data)] = [:]
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
                    let record = await Self.openPublic(
                        hit.publicRecord, output: oprf.output, accountId: hit.accountId,
                        identityKey: key, ports: ports)
                    let published = record?.displayName
                    let k = known(key)
                    guard let name = k.name ?? published ?? k.fallbackName,
                          nameMatchesQuery(displayName: name, query: query),
                          let r = result(
                              key: key, accountId: hit.accountId, known: k, foundReach: .open,
                              foundName: published, foundUsername: nil, seed: nil,
                              mailbox: Self.mailbox(of: record)) else { continue }
                    namedOnly.append(r)
                    if r.username == nil, let claimed = record?.username,
                       let certificate = record?.usernameCertificate {
                        nameClaims[r.accountKey] = (claimed, certificate, hit.accountId)
                    }
                }
            } catch {
                note(error)
            }
        }

        // A username a record states shows only once its certificate binds
        // its lookup key to the account. The typed one's key is at hand; any
        // other costs a token, so they are capped together. A failed token
        // costs the username, never the row.
        var extraChecks = 0
        func provenUsername(_ claimed: String, certificate: Data, accountId: Data) async -> String? {
            var lookupKey: Data?
            if let u = usernameOprf, claimed == u.canonical {
                lookupKey = u.oprf.lookupKey
            } else if extraChecks < maxExtraUsernameChecks {
                extraChecks += 1
                lookupKey = try? await ports.usernameLookupKey(claimed)
            }
            guard let lookupKey, await ports.verifyCertificate(certificate, lookupKey, accountId) else {
                return nil
            }
            return claimed
        }
        // The row of a name match shows the username as the row of a username
        // match does (UX section 6.1).
        var byName: [FoundPerson] = []
        for person in namedOnly {
            if let claim = nameClaims[person.accountKey],
               let proven = await provenUsername(
                   claim.username, certificate: claim.certificate, accountId: claim.accountId) {
                byName.append(person.withProvenUsername(proven))
            } else {
                byName.append(person)
            }
        }

        // 3. Contacts of contacts: the introduction records, matched locally.
        var byIntroduction: [FoundPerson] = []
        var recordsOpened = 0
        var recordsKept = 0
        for record in records {
            guard let (payload, _) = openRecord(
                record, nameOprf: nameOprf, usernameOprf: usernameOprf, query: query,
                onOpened: { recordsOpened += 1 }) else { continue }
            recordsKept += 1
            // An introduction record always carries the `fof` grant that
            // reaches its owner; one without it gives this device no way in.
            guard let fof = payload.fof else { continue }
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
                verified = await provenUsername(claimed, certificate: cert, accountId: payload.accountId)
            }
            let name = payload.displayName ?? verified.map { "@\($0)" }
                ?? displayFingerprint(personKey)
            if let r = result(
                key: personKey, accountId: payload.accountId, known: known(personKey),
                foundReach: .grant, foundName: name, foundUsername: verified, seed: fof.seed,
                mailbox: MailboxTarget.verifiedOrNil(grant: fof, accountKey: payload.identityPk),
                grant: fof) {
                byIntroduction.append(r)
            }
        }

        // One list, then the reach check: a narrowed setting takes effect at
        // the next search, not at the next rotation. A check that says no
        // drops the result silently.
        var found: [FoundPerson] = []
        var refused = 0
        for p in merge([byUsername, byName, byIntroduction], ownAccountKey: ownAccountKey) {
            guard p.reach == .grant else {
                found.append(p)
                continue
            }
            guard let seed = p.writeSeed, let mailbox = p.mailbox else { continue }
            do {
                if try await ports.reachCheck(mailbox, seed) {
                    found.append(p.passingReachCheck())
                } else {
                    refused += 1
                }
            } catch {
                failed = true
            }
        }
        // Counts only: never a name, a key, the query or a grant.
        NSLog(
            "search: records=%ld opened=%ld kept=%ld refused=%ld people=%ld",
            records.count, recordsOpened, recordsKept, refused, found.count)
        return PeopleNetworkLegs(people: found, allowanceUsed: allowance, failed: failed)
    }

    /// The channel namespace for the typed handle (UX section 6.1), asked
    /// only when the text could be a name: the channel, or `nil` when the
    /// handle names none, and whether the question was refused for a used-up
    /// allowance or got no answer.
    public static func channelLeg(
        query: String, lookup: (String) async throws -> String?
    ) async -> (channel: FoundChannel?, allowanceUsed: Bool, failed: Bool) {
        let bare = NameLookup.bareName(query.trimmingCharacters(in: .whitespacesAndNewlines))
        guard NameLookup.looksLikeAName(bare) else { return (nil, false, false) }
        do {
            guard let idHex = try await lookup(bare) else { return (nil, false, false) }
            return (FoundChannel(idHex: idHex, handle: bare), false, false)
        } catch {
            let quota = AnonTokenUnavailable.isQuotaExhausted(error)
            return (nil, quota, !quota)
        }
    }

    /// One network search of the field: the people (username, name term,
    /// introductions) and the channel handle, asked at the same time.
    static func answer(
        query: String,
        records: [Data],
        known: (Data) -> KnownPerson = { _ in .unknown },
        ownAccountKey: Data? = nil,
        ports: PeopleSearchPorts,
        lookupChannel: @escaping (String) async throws -> String?,
        searchConversations: (@Sendable (String) async throws -> [ConversationHandles.Found])? = nil
    ) async -> PeopleSearchAnswer {
        // ADR-0087 section 5 — one network query finds people, groups and
        // channels; the channel-only leg answers only without it.
        if let searchConversations {
            async let found: (list: [ConversationHandles.Found], quota: Bool, failed: Bool) = {
                do {
                    return (try await searchConversations(query), false, false)
                } catch {
                    let quota = AnonTokenUnavailable.isQuotaExhausted(error)
                    return ([], quota, !quota)
                }
            }()
            let legs = await networkLegs(
                query: query, records: records, known: known, ownAccountKey: ownAccountKey, ports: ports)
            let f = await found
            var answer = PeopleSearchAnswer(
                legs: legs, channel: nil, channelAllowanceUsed: f.quota, channelFailed: f.failed)
            answer.conversations = f.list
            return answer
        }
        async let channel = channelLeg(query: query, lookup: lookupChannel)
        let legs = await networkLegs(
            query: query, records: records, known: known, ownAccountKey: ownAccountKey, ports: ports)
        let c = await channel
        return PeopleSearchAnswer(
            legs: legs, channel: c.channel, channelAllowanceUsed: c.allowanceUsed, channelFailed: c.failed)
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
        // `who-can-find-me.md` 6.1, "One list" (2026-11-04): the whole list,
        // not people alone, so a search that found a group never reads "No
        // one found" above it.
        case .notFound: return Localization.text("search.nothing_found")
        case .failed: return Localization.text("search.people.failed")
        case .allowanceUsed: return Localization.text("search.people.out_of_searches")
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
            keptGrants: ReachCenter.shared.searchGrants(forAccountKeyB64: keyB64))
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

/// The search field's session (UX section 6.1), shared by the field at the top
/// of Chats and the one at the top of Contacts: the answers of the last
/// ``PeopleSearch/answerKeptSeconds``, the searches in flight, and each found
/// person's request state. A row says what is true now: nothing it says may
/// need a restart to become correct.
@MainActor
public final class PeopleSearchSession: ObservableObject {

    /// What the field needs from the app, injected so the rules run in a test.
    public struct Ports {
        /// The people this device already knows (contacts, members of its
        /// groups and channels), each with every grant kept for them.
        public var candidates: @MainActor () -> [LocalCandidate]
        /// This account's key: never a result.
        public var ownAccountKey: @MainActor () -> Data?
        /// One network search: username, name, introductions, channel handle.
        public var network: @MainActor (String) async -> PeopleSearchAnswer
        /// Is the key of a seed still registered on the mailbox given? Spends
        /// no anonymous token. Throws for a connection failure.
        public var reachCheck: @MainActor (MailboxTarget, Data) async throws -> Bool
        /// Send the contact request to a result, at once, with no
        /// introduction text.
        public var sendRequest: @MainActor (FoundPerson) async throws -> Void
        /// Whether the outgoing list (Contacts > Sent) holds this person, by
        /// account key or account id: the row then reads "Request sent", and
        /// only then.
        public var alreadyRequested: @MainActor (FoundPerson) -> Bool
        /// The row of this person under Contacts > Sent, `nil` for none (a
        /// person added through their invite link waits without one). A
        /// declined request says nothing to the sender, so this is what
        /// **Ask again** works from.
        public var sentRow: @MainActor (FoundPerson) -> SentContactRequest?
        /// **Dismiss** on that row.
        public var dismissSent: @MainActor (SentContactRequest) -> Void
        /// Every grant this device holds for an account key NOW, in the order
        /// they are tried. Read fresh: one may have arrived since the row.
        public var heldGrants: @MainActor (Data) -> [KeptGrant]
        /// Whether this person is a contact now.
        public var isContact: @MainActor (FoundPerson) -> Bool

        public init(
            candidates: @escaping @MainActor () -> [LocalCandidate],
            ownAccountKey: @escaping @MainActor () -> Data?,
            network: @escaping @MainActor (String) async -> PeopleSearchAnswer,
            reachCheck: @escaping @MainActor (MailboxTarget, Data) async throws -> Bool,
            sendRequest: @escaping @MainActor (FoundPerson) async throws -> Void,
            alreadyRequested: @escaping @MainActor (FoundPerson) -> Bool = { _ in false },
            sentRow: @escaping @MainActor (FoundPerson) -> SentContactRequest? = { _ in nil },
            dismissSent: @escaping @MainActor (SentContactRequest) -> Void = { _ in },
            heldGrants: (@MainActor (Data) -> [KeptGrant])? = nil,
            isContact: @escaping @MainActor (FoundPerson) -> Bool = { _ in false }
        ) {
            self.sentRow = sentRow
            self.dismissSent = dismissSent
            self.candidates = candidates
            self.ownAccountKey = ownAccountKey
            self.network = network
            self.reachCheck = reachCheck
            self.sendRequest = sendRequest
            self.alreadyRequested = alreadyRequested
            self.heldGrants = heldGrants ?? { key in
                candidates().first { $0.accountKey == key }?.known.keptGrants ?? []
            }
            self.isContact = isContact
        }
    }

    /// At most this many people are asked about at once: one letter may match
    /// everyone this device knows. Chosen, not measured.
    public static let maxConcurrentReachChecks = 4

    /// Request states by account key: a send under way, or the line it left
    /// (refused, failed), which lasts until the text changes. "Request sent"
    /// is not held here: it is read from the outgoing list.
    @Published public private(set) var requests: [Data: PersonRequestState] = [:]

    /// The answers about kept grants, by account key.
    public private(set) var verdicts: [Data: ReachVerdict] = [:]
    /// Fires after a verdict or a failed check changed what a row shows.
    let reachChanged = PassthroughSubject<Void, Never>()

    let ports: Ports
    private var answers: [String: (answer: PeopleSearchAnswer, at: TimeInterval)] = [:]
    /// A clock that never runs backwards, in seconds.
    private let uptime: @MainActor () -> TimeInterval
    /// The wall clock in milliseconds since the Unix epoch, which a Sent
    /// row's `createdAt` is compared with (injected for tests).
    private let nowMs: @MainActor () -> Int64
    private var inFlight: [String: Task<PeopleSearchAnswer, Never>] = [:]
    /// The people whose last check could not be asked.
    private var unansweredChecks: Set<Data> = []
    private var reachQueue: [LocalCandidate] = []
    private var reachChecks: [Data: Task<Void, Never>] = [:]

    public init(
        ports: Ports,
        uptime: @escaping @MainActor () -> TimeInterval = { ProcessInfo.processInfo.systemUptime },
        nowMs: @escaping @MainActor () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.ports = ports
        self.uptime = uptime
        self.nowMs = nowMs
    }

    /// The answer remembered for `key`, while it is young enough to reuse.
    public func remembered(_ key: String) -> PeopleSearchAnswer? {
        guard let held = answers[key] else { return nil }
        if uptime() - held.at < PeopleSearch.answerKeptSeconds, !namesFormerContact(held.answer) {
            return held.answer
        }
        answers[key] = nil
        return nil
    }

    /// Whether `answer` names as a contact someone who is no longer one. Such
    /// a row is wrong twice: the person has no way to be added from it, and
    /// in Contacts it is hidden as "listed above", where they no longer stand.
    func namesFormerContact(_ answer: PeopleSearchAnswer) -> Bool {
        answer.people.contains { $0.reach == .contact && !ports.isContact($0) }
    }

    /// The answer for `key`: the remembered one, the one already being asked,
    /// or a new network search. A search is never cancelled once asked (its
    /// tokens are spent), so a text typed away from and back to joins it.
    func answer(key: String, query: String) async -> PeopleSearchAnswer {
        if let held = remembered(key) { return held }
        if let running = inFlight[key] { return await running.value }
        let network = ports.network
        let task = Task { @MainActor in await network(query) }
        inFlight[key] = task
        let result = await task.value
        inFlight[key] = nil
        if result.remembered { answers[key] = (result, uptime()) }
        return result
    }

    /// The people this device knows matching `query`, with what the Delivery
    /// Service said about their kept grants.
    public func localPeople(_ query: String) -> [FoundPerson] {
        PeopleSearch.local(
            query: query,
            candidates: PeopleSearch.applying(
                verdicts: verdicts, unanswered: unansweredChecks, to: ports.candidates()),
            ownAccountKey: ports.ownAccountKey())
    }

    /// Ask about everyone matching `query` who holds grants, whatever verdict
    /// is held, the newest text first: the last answer stays on screen until
    /// this one arrives. Nobody is asked about twice at the same time,
    /// whichever field typed it.
    public func checkReach(query: String) {
        let needed = PeopleSearch.needingReachCheck(
            query: query, candidates: ports.candidates(),
            ownAccountKey: ports.ownAccountKey()
        ).filter { reachChecks[$0.accountKey] == nil }
        let keys = Set(needed.map(\.accountKey))
        reachQueue = needed + reachQueue.filter { !keys.contains($0.accountKey) }
        pumpReachChecks()
    }

    private func pumpReachChecks() {
        while reachChecks.count < Self.maxConcurrentReachChecks, !reachQueue.isEmpty {
            let candidate = reachQueue.removeFirst()
            let key = candidate.accountKey
            let grants = candidate.known.keptGrants
            guard reachChecks[key] == nil else { continue }
            let check = ports.reachCheck
            reachChecks[key] = Task { @MainActor [weak self] in
                let verdict = await PeopleSearch.reachVerdict(grants: grants) { mailbox, seed in
                    try await check(mailbox, seed)
                }
                self?.finishReachCheck(key, verdict)
            }
        }
    }

    private func finishReachCheck(_ key: Data, _ verdict: ReachVerdict?) {
        reachChecks[key] = nil
        if let verdict {
            verdicts[key] = verdict
            unansweredChecks.remove(key)
        } else {
            unansweredChecks.insert(key)
        }
        reachChanged.send()
        pumpReachChecks()
    }

    /// A send was refused: none of the grants kept for `person` reaches them.
    private func recordRefused(_ person: FoundPerson) {
        guard person.reach == .grant else { return }
        var seeds = Set(person.writeSeed.map { [$0] } ?? [])
        seeds.formUnion(ports.heldGrants(person.accountKey).map(\.seed))
        verdicts[person.accountKey] = .dead(checkedSeeds: seeds)
        unansweredChecks.remove(person.accountKey)
        reachChanged.send()
    }

    /// Wait for the checks under way (tests).
    func settleReachChecks() async {
        while let running = reachChecks.values.first {
            await running.value
        }
    }

    /// The row state of `person`: "Request sent" exactly while the outgoing
    /// list holds them. Declined and dismissed, or accepted, the row goes
    /// back to what the check says.
    public func requestState(for person: FoundPerson) -> PersonRequestState {
        if ports.alreadyRequested(person) { return .sent }
        return requests[person.accountKey] ?? .idle
    }

    /// The text changed: a refused or failed line goes, so the row reflects
    /// the new check. A request on its way stays claimed.
    public func textChanged() {
        let kept = requests.filter { $0.value == .sending }
        if kept.count != requests.count { requests = kept }
    }

    /// The contacts or the outgoing list changed: every row is read again.
    public func noteContactsChanged() {
        objectWillChange.send()
        reachChanged.send()
    }

    /// `person` with the grant the send writes with: the row's when it still
    /// passes, else any other this device holds for them now. None passing is
    /// a refusal; a check that could not be made throws.
    private func writable(_ person: FoundPerson) async throws -> FoundPerson {
        guard person.reach == .grant else { return person }
        let row: KeptGrant?
        if let seed = person.writeSeed, let mailbox = person.mailbox {
            row = KeptGrant(seed: seed, mailbox: mailbox)
        } else {
            row = nil
        }
        let check = ports.reachCheck
        guard let grant = try await PeopleSearch.reachingGrant(
            row: row, held: ports.heldGrants(person.accountKey),
            check: { mailbox, seed in try await check(mailbox, seed) }
        ) else {
            throw ContactsError.requestRefused
        }
        return person.writing(with: grant)
    }

    /// **Add contact**: send the request at once; the row reads "Request
    /// sent" once the outgoing list holds it, the refusal or the failure
    /// otherwise.
    public func addContact(_ person: FoundPerson) async {
        guard person.canAddContact, requestState(for: person).offersAdd else { return }
        requests[person.accountKey] = .sending
        do {
            try await ports.sendRequest(try await writable(person))
            requests[person.accountKey] = nil
        } catch {
            let state = PersonRequestState.after(error: error)
            requests[person.accountKey] = state
            if state == .refused { recordRefused(person) }
        }
    }

    /// Whether the row offers **Ask again**: an earlier request has stood
    /// unanswered under Sent for a day (the one ask-again rule,
    /// ``RequestsSentStore/mayAskAgain(sentAtMs:nowMs:)``). A decline tells
    /// the sender nothing, so the row keeps "Request sent" and this is the
    /// way on; right after Add contact the row reads "Request sent" and the
    /// button goes away (`docs/ux/who-can-find-me.md` section 6.1). A row
    /// whose time does not parse offers nothing.
    public func canAskAgain(_ person: FoundPerson) -> Bool {
        guard person.canAddContact, let row = ports.sentRow(person),
              let sentAtMs = DsTimestamp.parseMs(row.createdAt)
        else { return false }
        return RequestsSentStore.mayAskAgain(sentAtMs: sentAtMs, nowMs: nowMs())
    }

    /// **Ask again**: what Contacts > Sent > Dismiss and then Add contact do,
    /// in one tap.
    public func askAgain(_ person: FoundPerson) async {
        guard person.canAddContact, let row = ports.sentRow(person) else { return }
        ports.dismissSent(row)
        await addContact(person)
    }
}

/// One search field (UX section 6.1): local results from the first letter;
/// from three characters, after ``PeopleSearch/networkDebounceMs`` without
/// typing, the network under the heading People. The answer last shown stays
/// while the person keeps typing, until the new one arrives.
@MainActor
public final class PeopleSearchModel: ObservableObject {

    /// The people this device knows matching the text.
    @Published public private(set) var local: [FoundPerson] = []
    /// The network answer shown under People; `nil` hides the heading.
    @Published public private(set) var answer: PeopleSearchAnswer?
    /// A network search for the current text is under way.
    @Published public private(set) var searching = false
    /// Whether the current text asks the network at all (People then always
    /// answers, so no "No results" line is needed beside it).
    @Published public private(set) var asksNetwork = false

    public let session: PeopleSearchSession
    /// ADR-0099 step 4 / UX `reporting-and-moderation.md` section 3 — search
    /// does not list a suspended account. Injectable for tests.
    public var isSuspended: @MainActor (Data) -> Bool = { SuspensionMarks.shared.isSuspended($0) }
    private let sleep: @Sendable (UInt64) async throws -> Void
    private var query = ""
    private var generation = 0
    private var pending: Task<Void, Never>?
    private var reachSubscription: AnyCancellable?

    /// `sleep` waits the given milliseconds (injected for tests).
    public init(
        session: PeopleSearchSession,
        sleep: @escaping @Sendable (UInt64) async throws -> Void = { ms in
            try await Task.sleep(nanoseconds: ms * 1_000_000)
        }
    ) {
        self.session = session
        self.sleep = sleep
        // Sent on the main actor, and a subject calls its sink at once.
        // A verdict arrived, or the contacts or the outgoing list changed.
        reachSubscription = session.reachChanged.sink { [weak self] in
            MainActor.assumeIsolated {
                self?.refreshLocal()
                self?.askAgainIfAnswerIsFalse()
            }
        }
    }

    /// A contact removed with the text standing: the answer on screen calls
    /// them one, and nothing else would ask while the text stands.
    private func askAgainIfAnswerIsFalse() {
        guard let answer, session.namesFormerContact(answer),
              let key = PeopleSearch.searchKey(query) else { return }
        self.answer = nil
        ask(key: key, typed: query)
    }

    private func refreshLocal() {
        local = query.isEmpty ? [] : session.localPeople(query)
    }

    /// The field's text changed.
    public func update(query raw: String) {
        let typed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard typed != query else { return }
        query = typed
        generation += 1
        let current = generation
        pending?.cancel()
        pending = nil
        session.textChanged()
        refreshLocal()
        if !typed.isEmpty { session.checkReach(query: typed) }
        let key = PeopleSearch.searchKey(typed)
        asksNetwork = key != nil
        guard let key else {
            answer = nil
            searching = false
            return
        }
        if let held = session.remembered(key) {
            answer = held
            searching = false
            return
        }
        searching = false
        let sleep = self.sleep
        pending = Task { @MainActor [weak self] in
            do { try await sleep(PeopleSearch.networkDebounceMs) } catch { return }
            guard let self, !Task.isCancelled, self.generation == current else { return }
            await self.ask(key: key, typed: typed, generation: current)
        }
    }

    /// Ask for the current text now, with no pause for typing.
    private func ask(key: String, typed: String) {
        generation += 1
        let current = generation
        pending?.cancel()
        pending = Task { @MainActor [weak self] in
            await self?.ask(key: key, typed: typed, generation: current)
        }
    }

    private func ask(key: String, typed: String, generation current: Int) async {
        searching = true
        let result = await session.answer(key: key, query: typed)
        guard generation == current else { return }
        answer = result
        searching = false
    }

    /// Wait for the field's pending search, if any (tests).
    func settle() async {
        await pending?.value
        await session.settleReachChecks()
    }

    /// The rows the field shows: the local people first (with what the
    /// network learned about them merged in), then the people only the
    /// network found. `showsLocalContacts` is `false` in Contacts, whose list
    /// already shows the matching contacts.
    ///
    /// A person who became a contact while the text stands leaves the network
    /// part at once, whatever the remembered answer says of them.
    ///
    /// One person, one row (UX section 6.1). `listed` are the people a list
    /// row above shows, a contact or someone added through their invite link:
    /// they stand nowhere here, whatever their reach. `chatPeers` are the
    /// people whose one-to-one matched above: a contact among them is not
    /// listed twice, and someone who is no contact keeps their row, and the
    /// way to add them. A contact the list does not match yet (their name has
    /// not arrived) is in neither and keeps a row, so the field is never
    /// blank.
    public func rows(
        showsLocalContacts: Bool, listed: Set<Data> = [], chatPeers: Set<Data> = []
    ) -> (local: [FoundPerson], network: [FoundPerson]) {
        let merged = PeopleSearch.merge([local, answer?.people ?? []], ownAccountKey: nil).map { person in
            person.reach != .contact && session.ports.isContact(person) ? person.asContact : person
        }.filter { !listed.contains($0.accountKey) }
        // ADR-0099 step 4 — a person known suspended is dropped as their
        // standing arrives; nothing waits for the check.
        let visible = SuspensionMarkRules.searchVisible(merged, key: \.accountKey, isSuspended: isSuspended)
        let localKeys = Set(local.map(\.accountKey))
        func isLocal(_ person: FoundPerson) -> Bool {
            localKeys.contains(person.accountKey) || person.reach == .contact
        }
        func listedAbove(_ person: FoundPerson) -> Bool {
            person.reach == .contact && (!showsLocalContacts || chatPeers.contains(person.accountKey))
        }
        return (
            visible.filter { isLocal($0) && !listedAbove($0) },
            visible.filter { !isLocal($0) })
    }

    /// ADR-0099 step 4 — every person the current answer names, before the
    /// suspension filter: what the screen asks the log about.
    public var suspensionCheckKeys: [Data] {
        (local + (answer?.people ?? [])).map(\.accountKey)
    }

    /// The network part's state, `nil` while nothing was answered.
    public func networkState(includesChannels: Bool) -> PeopleNetworkState? {
        answer?.state(includesChannels: includesChannels)
    }

    /// The state line at the end of the one list (`who-can-find-me.md` 6.1,
    /// "One list"). "Nothing found" says nothing stands in the field, so it
    /// goes whenever any row does: a person here, or a row of the screen's
    /// own list above (`listShowsRows`).
    public func peopleMessage(
        includesChannels: Bool, showsLocalContacts: Bool, listed: Set<Data> = [],
        chatPeers: Set<Data> = [], listShowsRows: Bool = false
    ) -> String? {
        let state = networkState(includesChannels: includesChannels)
        let rows = rows(showsLocalContacts: showsLocalContacts, listed: listed, chatPeers: chatPeers)
        if state == .notFound, listShowsRows || !rows.local.isEmpty || !rows.network.isEmpty { return nil }
        return state?.message
    }

    /// Whether the field shows anything of its own (a row, the spinner or a
    /// line under People).
    public func showsAnything(
        includesChannels: Bool, showsLocalContacts: Bool, listed: Set<Data> = [],
        chatPeers: Set<Data> = []
    ) -> Bool {
        if searching { return true }
        let rows = rows(showsLocalContacts: showsLocalContacts, listed: listed, chatPeers: chatPeers)
        if !rows.local.isEmpty || !rows.network.isEmpty { return true }
        if includesChannels, answer?.channel != nil || !(answer?.conversations.isEmpty ?? true) { return true }
        if let state = networkState(includesChannels: includesChannels), state != .found { return true }
        return false
    }
}
