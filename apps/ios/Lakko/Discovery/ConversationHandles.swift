// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 sections 2 to 4 — a group's or channel's `@name` at discovery,
/// the iOS twin of web `chat/group-handle.ts` / Android `GroupHandle.kt`.
///
/// The write key names no account: `handleWriteSeed(journal root seed)`, the
/// root being ADR-0057's from the OWNER's account seed and the journal id (a
/// group id, or a channel's PublisherGroup id), so only the owner writes and
/// every device of the owner derives it. A claim and a replace spend one
/// `discovery-register` token.
enum ConversationHandles {

    /// What a public name's record says (section 4).
    struct PublicFacts {
        let kind: FfiHandleRecordKind
        /// The group id (32) or channel id (16).
        let id: Data
        /// The group's knock public key, or the channel's read key.
        let key: Data
        /// The version of the group's knock key (0 for a channel).
        var knockVersion: UInt16 = 0
        let name: String
        let description: String
        let size: Int
        let avatar: FfiAvatar?
    }

    /// The ADR-0059 size band of a count (0..5).
    static func sizeBand(_ count: Int) -> UInt8 {
        switch count {
        case 100_000...: return 5
        case 10_000...: return 4
        case 1_000...: return 3
        case 100...: return 2
        case 10...: return 1
        default: return 0
        }
    }

    /// Section 2 — a typed `@name` without its `@`, lower case.
    static func canonical(_ typed: String) -> String {
        String(typed.trimmingCharacters(in: .whitespaces).drop(while: { $0 == "@" })).lowercased()
    }

    static func wellFormed(_ typed: String) -> Bool {
        (try? validateUsername(input: canonical(typed))) != nil
    }

    /// The handle write seed of a journal this account owns. The caller wipes.
    static func seed(accountSeed: Data, journalId: Data) throws -> Data {
        var root = try dsJournalRootSeed(accountIdentitySeed: accountSeed, journalId: journalId)
        defer { RootHandover.wipe(&root) }
        return try handleWriteSeed(journalRootSeed: root)
    }

    /// The name search terms a public record publishes, after the `@name`.
    private static func recordTerms(_ facts: PublicFacts) -> [String] {
        let max = Int(maxHandleTerms())
        return Array(nameSearchTerms(displayName: facts.name).prefix(Swift.max(0, max - 1)))
    }

    /// The `@name`s and, for a public record, its name terms, evaluated in
    /// ONE `/discovery/oprf/batch` request (one `discovery-register` token):
    /// the results of `names` in order, then those of the terms.
    private static func evaluateOwn(
        discovery: DiscoveryClient, names: [String], facts: PublicFacts?
    ) async throws -> (names: [OprfResult], terms: [OprfResult]) {
        let terms = facts.map(recordTerms) ?? []
        let inputs = names.map(DiscoveryClient.OwnOprfInput.name) + terms.map(DiscoveryClient.OwnOprfInput.term)
        // At most the old and the new `@name` plus 64 terms: 66, the batch
        // cap, so nothing is dropped.
        let results = try await discovery.oprfOwnBatch(inputs)
        return (Array(results.prefix(names.count)), Array(results.dropFirst(names.count)))
    }

    /// The record and the term lookup keys a public name publishes.
    private static func publicContent(
        handle: String, handleOprf: OprfResult, termOprfs: [OprfResult], facts: PublicFacts
    ) throws -> (record: Data, terms: [Data]) {
        let outputs = [handleOprf.output] + termOprfs.map(\.output)
        let lookupKeys = [handleOprf.lookupKey] + termOprfs.map(\.lookupKey)
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        let payload = FfiHandleRecordPayload(
            kind: facts.kind, updatedAtMs: now, id: facts.id, key: facts.key,
            knockVersion: facts.kind == .channel ? 0 : facts.knockVersion, handle: handle,
            name: facts.name, description: facts.description, sizeBand: sizeBand(facts.size),
            avatar: facts.avatar)
        let record = try composeHandleRecord(payload: payload, termOprfs: outputs, issuedAtMs: now)
        return (record, lookupKeys)
    }

    struct Written {
        let handle: String
        let isPublic: Bool
        let certificate: Data
        let leaseUntilMs: UInt64
    }

    /// Take `handle` (create, or a lapsed name).
    static func claim(
        discovery: DiscoveryClient, accountSeed: Data, journalId: Data,
        handle typed: String, isPublic: Bool, facts: PublicFacts?
    ) async throws -> Written {
        try await write(
            route: .claim, discovery: discovery, accountSeed: accountSeed, journalId: journalId,
            handle: typed, isPublic: isPublic, facts: facts)
    }

    /// Flip public / private, or republish the record and renew the lease.
    static func update(
        discovery: DiscoveryClient, accountSeed: Data, journalId: Data,
        handle typed: String, isPublic: Bool, facts: PublicFacts?
    ) async throws -> Written {
        try await write(
            route: .update, discovery: discovery, accountSeed: accountSeed, journalId: journalId,
            handle: typed, isPublic: isPublic, facts: facts)
    }

    private static func write(
        route: DiscoveryClient.HandleRoute, discovery: DiscoveryClient, accountSeed: Data,
        journalId: Data, handle typed: String, isPublic: Bool, facts: PublicFacts?
    ) async throws -> Written {
        let name = canonical(typed)
        let published = isPublic ? facts : nil
        let evaluated = try await evaluateOwn(discovery: discovery, names: [name], facts: published)
        let pair = evaluated.names[0]
        var seed = try seed(accountSeed: accountSeed, journalId: journalId)
        defer { RootHandover.wipe(&seed) }
        var record: Data?
        var terms: [Data] = []
        if let published {
            (record, terms) = try publicContent(
                handle: name, handleOprf: pair, termOprfs: evaluated.terms, facts: published)
        }
        let hash = try handleContentHash(record: record, terms: terms)
        let visibility: FfiHandleVisibility = isPublic ? .public : .private
        let body: FfiHandleStatementBody = route == .claim
            ? .claim(visibility: visibility, contentHash: hash)
            : .update(visibility: visibility, contentHash: hash)
        let statement = try signHandleStatement(handleSeed: seed, lookupKey: pair.lookupKey, body: body)
        guard let answer = try await discovery.handleWrite(
            route: route, statementB64: statement, record: record, terms: terms) else {
            throw DiscoveryClientError.http(status: 200, endpoint: "/discovery/handle", body: "no certificate")
        }
        return Written(
            handle: name, isPublic: isPublic, certificate: answer.certificate,
            leaseUntilMs: answer.leaseUntilMs)
    }

    /// The handle write seed derived from a given account seed (an ADR-0074
    /// predecessor's, for the rekey). The caller wipes.
    static func rekey(
        discovery: DiscoveryClient, oldAccountSeed: Data, newAccountSeed: Data, journalId: Data,
        handle typed: String
    ) async throws -> Written {
        let name = canonical(typed)
        let pair = try await evaluateOwn(discovery: discovery, names: [name], facts: nil).names[0]
        var old = try seed(accountSeed: oldAccountSeed, journalId: journalId)
        defer { RootHandover.wipe(&old) }
        var new = try seed(accountSeed: newAccountSeed, journalId: journalId)
        defer { RootHandover.wipe(&new) }
        let statement = try signHandleStatement(
            handleSeed: old, lookupKey: pair.lookupKey,
            body: .rekey(newWritePk: try handleWritePublicKey(handleSeed: new)))
        guard let answer = try await discovery.handleWrite(route: .rekey, statementB64: statement) else {
            throw DiscoveryClientError.http(status: 200, endpoint: "/discovery/handle/rekey", body: "no certificate")
        }
        return Written(handle: name, isPublic: false, certificate: answer.certificate, leaseUntilMs: answer.leaseUntilMs)
    }

    /// Move to a new name; the old one is held 30 days (one
    /// `discovery-register` token).
    static func replace(
        discovery: DiscoveryClient, accountSeed: Data, journalId: Data, oldHandle: String,
        newHandle typed: String, isPublic: Bool, facts: PublicFacts?
    ) async throws -> Written {
        let name = canonical(typed)
        let published = isPublic ? facts : nil
        let evaluated = try await evaluateOwn(
            discovery: discovery, names: [canonical(oldHandle), name], facts: published)
        let old = evaluated.names[0]
        let next = evaluated.names[1]
        var seed = try seed(accountSeed: accountSeed, journalId: journalId)
        defer { RootHandover.wipe(&seed) }
        var record: Data?
        var terms: [Data] = []
        if let published {
            (record, terms) = try publicContent(
                handle: name, handleOprf: next, termOprfs: evaluated.terms, facts: published)
        }
        let hash = try handleContentHash(record: record, terms: terms)
        let statement = try signHandleStatement(
            handleSeed: seed, lookupKey: old.lookupKey,
            body: .replace(newLookupKey: next.lookupKey, visibility: isPublic ? .public : .private, contentHash: hash))
        guard let answer = try await discovery.handleWrite(
            route: .replace, statementB64: statement, record: record, terms: terms) else {
            throw DiscoveryClientError.http(status: 200, endpoint: "/discovery/handle/replace", body: "no certificate")
        }
        return Written(handle: name, isPublic: isPublic, certificate: answer.certificate, leaseUntilMs: answer.leaseUntilMs)
    }

    /// Retire the name (Delete group, section 11).
    static func retire(discovery: DiscoveryClient, accountSeed: Data, journalId: Data, handle typed: String) async throws {
        let pair = try await evaluateOwn(discovery: discovery, names: [canonical(typed)], facts: nil).names[0]
        var seed = try seed(accountSeed: accountSeed, journalId: journalId)
        defer { RootHandover.wipe(&seed) }
        let statement = try signHandleStatement(handleSeed: seed, lookupKey: pair.lookupKey, body: .retire)
        _ = try await discovery.handleWrite(route: .retire, statementB64: statement)
    }

    /// A public group or channel found by search (section 5).
    struct Found: Equatable, Identifiable {
        let kind: FfiHandleRecordKind
        let id: Data
        let key: Data
        let knockVersion: UInt16
        let handle: String
        let name: String
        let description: String
        let sizeBand: UInt8
        let avatar: ProfileAvatar?

        var idHex: String { AttachmentBridge.hex(id) }
    }

    static func open(record: Data, oprfOutput: Data) -> Found? {
        guard let p = (try? openHandleRecord(record: record, oprfOutput: oprfOutput)) ?? nil else { return nil }
        return Found(
            kind: p.kind, id: p.id, key: p.key, knockVersion: p.knockVersion, handle: p.handle,
            name: p.name, description: p.description, sizeBand: p.sizeBand,
            avatar: ProfileAvatar(p.avatar))
    }

    /// Section 5 — public groups and channels for a typed query: `@name`
    /// exactly, and the longest word by its name terms (from 3 letters).
    /// Two tokens at most. The client filters by the whole query.
    static func search(discovery: DiscoveryClient, query: String) async throws -> [Found] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard q.count >= 3 else { return [] }
        var out: [String: Found] = [:]
        var order: [String] = []
        func keep(_ found: Found) {
            guard out[found.idHex] == nil else { return }
            out[found.idHex] = found
            order.append(found.idHex)
        }
        let asHandle = canonical(q)
        var lastError: Error?
        if wellFormed(asHandle) {
            do {
                if let found = try await lookupExact(discovery: discovery, handle: asHandle) { keep(found) }
            } catch {
                lastError = error
            }
        }
        if !q.hasPrefix("@") {
            let words: [String] = q.components(separatedBy: .whitespaces).filter { !$0.isEmpty }
            let word = words.max(by: { $0.count < $1.count }) ?? ""
            if let term = nameSearchTerms(displayName: word).max(by: { $0.count < $1.count }) {
                do {
                    let pair = try await discovery.oprfNameTerm(term)
                    for record in try await discovery.nameLookupHandles(termLookupKey: pair.lookupKey) {
                        guard let found = open(record: record, oprfOutput: pair.output) else { continue }
                        if found.name.localizedCaseInsensitiveContains(q) || found.handle.contains(asHandle) {
                            keep(found)
                        }
                    }
                } catch {
                    lastError = error
                }
            }
        }
        if out.isEmpty, let lastError { throw lastError }
        return order.compactMap { out[$0] }
    }

    /// The public group or channel holding exactly `@name`, or `nil`.
    static func lookupExact(discovery: DiscoveryClient, handle typed: String) async throws -> Found? {
        let name = canonical(typed)
        guard wellFormed(name) else { return nil }
        let pair = try await discovery.oprfName(name)
        guard let record = try await discovery.lookupHandleRecord(lookupKey: pair.lookupKey) else { return nil }
        return open(record: record, oprfOutput: pair.output)
    }

    /// The handle a channel claims (ADR-0087 section 3, the rule all three
    /// clients follow): the journal is the channel's PublisherGroup, whose id
    /// and the channel's read key come from the account's LINEAGE identity
    /// (the oldest own predecessor after a rotation, so neither moves with
    /// one); the write key is then `handleWriteSeed(journal root)` over the
    /// CURRENT account seed (``seed(accountSeed:journalId:)``), and a
    /// rotation queues a `rekey` (``HeldHandleStore/markRekeyOwed``).
    /// Published with the read key so a subscriber can find it (section 6).
    static func channelFacts(
        lineageIdentity: Identity, channelId: Data, name: String, description: String,
        avatar: FfiAvatar? = nil, size: Int = 0
    ) throws -> (journalId: Data, facts: PublicFacts) {
        let journal = try publisherGroupIdFor(ownerIdentity: lineageIdentity, channelId: channelId)
        let readKey = try channelReadKey(ownerAccountIdentity: lineageIdentity, channelId: channelId)
        return (journal, PublicFacts(
            kind: .channel, id: channelId, key: readKey, name: name, description: description,
            size: size, avatar: avatar))
    }
}

/// ADR-0087 section 3 — the names this account's devices hold, kept so the
/// owner's device renews each before its lease lapses. Per account, in
/// `UserDefaults`: a name, a journal id, a channel id, a lease end; no secret.
public enum HeldHandleStore {

    struct Entry: Codable, Equatable {
        /// `group` or `channel`.
        let kind: String
        let handle: String
        let isPublic: Bool
        /// Hex of the journal the handle key is derived for.
        let journalIdHex: String
        /// Hex of the channel id (a channel only).
        let channelIdHex: String?
        let name: String
        let description: String
        var leaseUntilMs: UInt64
        /// A republish is owed (a knock-version bump changed the public
        /// record, ADR-0087 section 5): written durably at the moment of the
        /// bump, cleared once discovery took the update.
        var owed: Bool? = nil
        /// An ADR-0074 rotation moved the handle key: the predecessor account
        /// key (base64) whose root signs the owed `rekey` (durable).
        var rekeyFromB64: String? = nil
    }

    static func key(_ accountIdB64: String) -> String { "lakko.held-handles.v1.\(accountIdB64)" }

    static func load(_ accountIdB64: String, _ defaults: UserDefaults = .standard) -> [Entry] {
        defaults.data(forKey: key(accountIdB64)).flatMap { try? JSONDecoder().decode([Entry].self, from: $0) } ?? []
    }

    static func save(_ entries: [Entry], _ accountIdB64: String, _ defaults: UserDefaults = .standard) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(try? JSONEncoder().encode(entries), forKey: key(accountIdB64))
    }

    static func put(_ entry: Entry, _ accountIdB64: String) {
        var all = load(accountIdB64).filter { $0.journalIdHex != entry.journalIdHex }
        all.append(entry)
        save(all, accountIdB64)
    }

    static func remove(journalIdHex: String, _ accountIdB64: String) {
        save(load(accountIdB64).filter { $0.journalIdHex != journalIdHex }, accountIdB64)
    }

    /// Section 3 — renew a name with fewer than this left.
    static let renewWithinMs: UInt64 = 90 * 86_400_000
    /// At most this many renewals per foreground session.
    static let maxRenewalsPerSession = 20

    /// The entries due for renewal now, at most `maxRenewalsPerSession`.
    static func due(_ entries: [Entry], nowMs: UInt64) -> [Entry] {
        Array(entries.filter {
            $0.rekeyFromB64 == nil && ($0.owed == true || $0.leaseUntilMs <= nowMs &+ renewWithinMs)
        }.prefix(maxRenewalsPerSession))
    }

    /// Record an owed rekey on every held name (an ADR-0074 rotation).
    static func markRekeyOwed(fromB64: String, _ accountIdB64: String) {
        var all = load(accountIdB64)
        for index in all.indices where all[index].rekeyFromB64 == nil { all[index].rekeyFromB64 = fromB64 }
        save(all, accountIdB64)
    }

    /// Record a republish as owed for this journal's name (durable).
    /// `group-and-channel-privacy.md` 4.5.4 — a channel's name or
    /// description changed (its owner's edit or a co-publisher's record read
    /// here): the held entry takes the new text and a republish is owed.
    /// `true` when this account holds the channel's name and it is public.
    @discardableResult
    static func noteChannelMeta(
        channelIdHex: String, name: String, description: String, _ accountIdB64: String
    ) -> Bool {
        var all = load(accountIdB64)
        guard let index = all.firstIndex(where: {
            $0.kind == "channel" && $0.channelIdHex?.lowercased() == channelIdHex.lowercased()
        }), all[index].isPublic else { return false }
        let old = all[index]
        guard old.name != name || old.description != description || old.owed == true else { return false }
        all[index] = Entry(
            kind: old.kind, handle: old.handle, isPublic: old.isPublic, journalIdHex: old.journalIdHex,
            channelIdHex: old.channelIdHex, name: name, description: description,
            leaseUntilMs: old.leaseUntilMs, owed: true, rekeyFromB64: old.rekeyFromB64)
        save(all, accountIdB64)
        return true
    }

    static func markOwed(journalIdHex: String, _ accountIdB64: String) {
        var all = load(accountIdB64)
        guard let index = all.firstIndex(where: { $0.journalIdHex == journalIdHex }) else { return }
        all[index].owed = true
        save(all, accountIdB64)
    }
}

/// Section 3 — the owner's device renews its names quietly while it is used.
@MainActor
final class HandleLeaseRenewal {
    static let shared = HandleLeaseRenewal()
    private var ranThisSession = false

    /// Renew what is due, once per foreground session. Best-effort: a failure
    /// is retried next session.
    /// Renew what is due once per foreground session; an owed republish is
    /// tried on every call (`force`).
    func runIfDue(
        accountIdB64: String?, accountIdentity: () -> Identity?,
        groupFacts: (String) -> ConversationHandles.PublicFacts? = { _ in nil },
        force: Bool = false
    ) async {
        guard force || !ranThisSession, let accountIdB64, let owner = accountIdentity() else { return }
        if !force { ranThisSession = true }
        let discovery = DiscoveryClient()
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        var entries = HeldHandleStore.due(HeldHandleStore.load(accountIdB64), nowMs: now)
        if force { entries = entries.filter { $0.owed == true } }
        for entry in entries {
            // A group's owed write (a flag change, a republish) is the
            // repository's: it also re-sends the in-group value.
            if entry.kind == "group" && entry.owed == true { continue }
            guard let journal = ChatRepository.hexToBytes(entry.journalIdHex) else { continue }
            var facts: ConversationHandles.PublicFacts?
            if entry.isPublic {
                if entry.kind == "channel", let channelHex = entry.channelIdHex,
                   let channelId = ChatRepository.hexToBytes(channelHex) {
                    facts = try? ConversationHandles.channelFacts(
                        lineageIdentity: ChannelLineage.identity(accountIdB64: accountIdB64, currentSeed: nil) ?? owner,
                        channelId: channelId, name: entry.name,
                        description: entry.description).facts
                } else if entry.kind == "group" {
                    facts = groupFacts(entry.journalIdHex)
                    // A public name needs its record: without one the write
                    // would turn it private. Kept owed until facts exist.
                    if facts == nil { continue }
                }
            }
            do {
                let written = try await ConversationHandles.update(
                    discovery: discovery, accountSeed: owner.ed25519Seed(), journalId: journal,
                    handle: entry.handle, isPublic: entry.isPublic, facts: facts)
                var renewed = entry
                renewed.leaseUntilMs = written.leaseUntilMs
                renewed.owed = nil
                HeldHandleStore.put(renewed, accountIdB64)
            } catch {
                NSLog("[handle] WARN a name renewal failed; retried later")
            }
        }
    }

    func resetSession() { ranThisSession = false }
}

/// `who-can-find-me.md` section 6.1 and `group-and-channel-privacy.md` 3.3 —
/// the live "taken" check of an `@name` field (New group, New channel, Group
/// type). The field asks 500 ms after typing stops; a text asked in the last
/// 60 seconds is not asked again, a found answer and a free one alike, so
/// typing back and forth spends no discovery-lookup token twice and does not
/// run into discovery's per-address limit (on web ten checks in a row met a
/// 429 and the create that followed failed). A create reuses a `taken`
/// answer instead of claiming; a `free` one never skips the claim.
@MainActor
enum HandleTakenCheck {
    enum Answer: Equatable {
        /// A public group, channel or person holds it.
        case taken
        /// No lookup answers for it (a private name still may hold it).
        case free
        /// Discovery said 429, or the lookup allowance is used up.
        case rateLimited
        /// Offline or another failure: nothing is said.
        case unknown
    }

    /// The pause after the last keystroke before the field asks.
    static let debounceNanoseconds: UInt64 = 500_000_000
    /// How long an answer is reused for the same text.
    static let reuseSeconds: TimeInterval = 60

    private static var answers: [String: (answer: Answer, at: Date)] = [:]

    /// The answer held for `handle` from the last 60 seconds, `taken` or
    /// `free` only.
    static func held(_ handle: String, now: Date = Date()) -> Answer? {
        let key = ConversationHandles.canonical(handle)
        guard let entry = answers[key], now.timeIntervalSince(entry.at) < reuseSeconds else { return nil }
        return entry.answer
    }

    /// Records what a claim learned: refused as taken, or now held by this
    /// account (taken as well, to anyone who asks).
    static func note(_ handle: String, _ answer: Answer, now: Date = Date()) {
        guard answer == .taken || answer == .free else { return }
        answers[ConversationHandles.canonical(handle)] = (answer, now)
    }

    /// One check: the held answer when there is one, else one OPRF round and
    /// one lookup.
    static func check(_ handle: String, discovery: DiscoveryClient = DiscoveryClient()) async -> Answer {
        let key = ConversationHandles.canonical(handle)
        if let held = held(key) { return held }
        do {
            let pair = try await discovery.oprfName(key)
            let answered = try await discovery.lookupRaw(LookupRequest(oprfHash: base64Encode(pair.lookupKey)))
            let answer: Answer = answered != nil ? .taken : .free
            note(key, answer)
            return answer
        } catch DiscoveryClientError.rateLimited {
            return .rateLimited
        } catch let DiscoveryClientError.http(status, _, _) where status == 429 {
            return .rateLimited
        } catch let token as AnonTokenUnavailable {
            return AnonTokenUnavailable.isQuotaExhausted(token) ? .rateLimited : .unknown
        } catch {
            return .unknown
        }
    }
}

/// ADR-0087 section 3 / ADR-0078 section 6 — the identity a channel's
/// PublisherGroup id and read key are derived from: the account's lineage
/// seed, the same one ``ChannelPublisherIdentity`` runs the channel provider
/// on.
enum ChannelLineage {
    /// `currentSeed` is the account's current phrase seed (what
    /// `Identity.fromSeed` takes), `nil` to read it from the seed store.
    static func identity(accountIdB64: String?, currentSeed: Data?) -> Identity? {
        guard let accountIdB64 else { return currentSeed.flatMap { try? Identity.fromSeed(seed: $0) } }
        guard let lineage = DeviceGroupLifecycle.lineageSeed(accountIdB64: accountIdB64, currentSeed: currentSeed)
            ?? currentSeed else { return nil }
        return try? Identity.fromSeed(seed: lineage)
    }

    /// The channel's read key, from the lineage identity.
    static func readKey(accountIdB64: String?, currentSeed: Data?, channelId: Data) -> Data? {
        guard let lineage = identity(accountIdB64: accountIdB64, currentSeed: currentSeed) else { return nil }
        return try? channelReadKey(ownerAccountIdentity: lineage, channelId: channelId)
    }
}
