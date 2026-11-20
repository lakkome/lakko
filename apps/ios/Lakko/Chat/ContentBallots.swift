// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

// ADR-0092 section 3.6 — anonymous poll ballots: the device's queue (a
// random 0..=10 s wait, survives a restart, retried at the next open), the
// proof computed off the main actor, and the account's own choice kept in the
// DeviceGroup synced register.

/// ADR-0092 section 3.4a — no ballot is posted in a run before its session
/// landing succeeded (an old device kept offline across a reset must not
/// vote under the old key).
public enum BallotGate {
    @MainActor public static var sessionLandedThisRun = false
}

/// The pure part of the queue: when an entry is due, and what a retry does.
public enum BallotSchedule {
    /// The ADR's posting delay, `0..=10 s`, uniform.
    public static let maxDelayMs: Int64 = 10_000

    public static func dueAt(nowMs: Int64, random: Int64) -> Int64 {
        nowMs + (random % (maxDelayMs + 1) + (maxDelayMs + 1)) % (maxDelayMs + 1)
    }

    /// The milliseconds to wait before posting an entry due at `dueAtMs`.
    public static func wait(dueAtMs: Int64, nowMs: Int64) -> Int64 {
        max(0, dueAtMs - nowMs)
    }
}

/// One queued ballot, persisted per account.
public struct QueuedBallot: Codable, Equatable {
    public var groupIdHex: String
    public var pollMessageId: Int64
    public var choices: [UInt8]
    public var dueAtMs: Int64
    public var failed: Bool

    public var key: String { "\(groupIdHex):\(pollMessageId)" }
}

/// What a poll card shows while its ballot is on its way.
public enum BallotSendState: Equatable {
    case sending
    case failed
}

/// The device's ballot queue.
@MainActor
public final class BallotQueue: ObservableObject {
    public static let shared = BallotQueue()

    @Published public private(set) var states: [String: BallotSendState] = [:]

    private weak var chat: ChatRepository?
    private var accountIdB64: String?
    private var entries: [QueuedBallot] = []
    private var running: Set<String> = []
    /// Injected in tests.
    var randomMs: () -> Int64 = { Int64.random(in: 0...BallotSchedule.maxDelayMs) }
    var nowMs: () -> Int64 = { ChatRepository.nowMs() }
    var sleep: (Int64) async -> Void = { ms in try? await Task.sleep(nanoseconds: UInt64(ms) * 1_000_000) }
    /// Posts one ballot; injected in tests. Returns the set version used.
    var post: ((QueuedBallot) async throws -> Void)?

    nonisolated static func storeKey(_ account: String) -> String { "lakko.poll-ballot-queue.v1.\(account)" }

    /// Bind to the account's chat repository and resume every queued ballot
    /// (the "retried at the next open" of ADR-0092 section 3.6).
    public func bind(chat: ChatRepository?, accountIdB64: String) {
        self.chat = chat
        self.accountIdB64 = accountIdB64
        entries = Self.load(accountIdB64)
        states = [:]
        for i in entries.indices {
            entries[i].failed = false
            states[entries[i].key] = .sending
        }
        save()
        for entry in entries { run(entry.key) }
    }

    public func state(groupIdHex: String, pollMessageId: Int64) -> BallotSendState? {
        states["\(groupIdHex):\(pollMessageId)"]
    }

    /// Queue a ballot (a new vote replaces a queued one for the same poll).
    public func enqueue(groupIdHex: String, pollMessageId: Int64, choices: [UInt8]) {
        let entry = QueuedBallot(
            groupIdHex: groupIdHex, pollMessageId: pollMessageId, choices: choices,
            dueAtMs: BallotSchedule.dueAt(nowMs: nowMs(), random: randomMs()), failed: false)
        entries.removeAll { $0.key == entry.key }
        entries.append(entry)
        states[entry.key] = .sending
        save()
        run(entry.key)
    }

    /// Walk-3 rule W3-R5 — the conversation is back in step: every ballot
    /// that failed in it goes again, as `Try again` would.
    public func retryFailed(groupIdHex: String) {
        for entry in entries where entry.groupIdHex == groupIdHex && entry.failed {
            retry(groupIdHex: groupIdHex, pollMessageId: entry.pollMessageId)
        }
    }

    /// `Try again` on a failed ballot: a fresh delay.
    public func retry(groupIdHex: String, pollMessageId: Int64) {
        let key = "\(groupIdHex):\(pollMessageId)"
        guard let i = entries.firstIndex(where: { $0.key == key }) else { return }
        entries[i].failed = false
        entries[i].dueAtMs = BallotSchedule.dueAt(nowMs: nowMs(), random: randomMs())
        states[key] = .sending
        save()
        run(key)
    }

    private func run(_ key: String) {
        guard !running.contains(key) else { return }
        running.insert(key)
        Task { @MainActor in
            defer { running.remove(key) }
            while let entry = entries.first(where: { $0.key == key }), !entry.failed {
                await sleep(BallotSchedule.wait(dueAtMs: entry.dueAtMs, nowMs: nowMs()))
                guard let current = entries.first(where: { $0.key == key }), !current.failed else { return }
                // A newer vote replaced this one while waiting: wait for its
                // own delay. Decided by what was replaced, never by the clock,
                // so an early wake cannot spin here.
                if current.dueAtMs != entry.dueAtMs || current.choices != entry.choices { continue }
                do {
                    try await (post ?? defaultPost)(current)
                    entries.removeAll { $0.key == key }
                    states[key] = nil
                    save()
                    chat?.objectWillChangeContent()
                    return
                } catch {
                    if let i = entries.firstIndex(where: { $0.key == key }) { entries[i].failed = true }
                    states[key] = .failed
                    save()
                    return
                }
            }
        }
    }

    /// Cast (off the main actor), post, record the own choice.
    private func defaultPost(_ entry: QueuedBallot) async throws {
        guard BallotGate.sessionLandedThisRun, let chat,
              let inputs = chat.ballotInputs(groupIdHex: entry.groupIdHex, pollMessageId: entry.pollMessageId),
              inputs.open else {
            throw ContentSendError.notReady
        }
        let center = PollChoiceCenter.shared
        let recorded = center.setVersion(groupIdHex: entry.groupIdHex, pollMessageId: entry.pollMessageId)
        let earlier = recorded ?? ((try? ownPollBallotSetVersion(
            keys: inputs.keys, voter: inputs.voter, verified: inputs.verified)) ?? nil)
        let choices = Data(entry.choices)
        let keys = inputs.keys, ring = inputs.ring, voter = inputs.voter
        let cast = try await Task.detached(priority: .userInitiated) {
            try castPollBallot(keys: keys, ring: ring, voter: voter, choices: choices, setVersion: earlier)
        }.value
        try await chat.postBallotRow(groupIdHex: entry.groupIdHex, row: cast.row)
        await center.record(
            groupIdHex: entry.groupIdHex, pollMessageId: entry.pollMessageId, choices: entry.choices,
            setVersion: cast.setVersion)
    }

    private func save() {
        guard let accountIdB64, let data = try? JSONEncoder().encode(entries) else { return }
        UserDefaults.standard.set(data, forKey: Self.storeKey(accountIdB64))
    }

    static func load(_ account: String) -> [QueuedBallot] {
        guard let data = UserDefaults.standard.data(forKey: storeKey(account)) else { return [] }
        return (try? JSONDecoder().decode([QueuedBallot].self, from: data)) ?? []
    }

    /// Forget the queue (Forget device, account switch).
    public func reset() {
        if let accountIdB64 { UserDefaults.standard.removeObject(forKey: Self.storeKey(accountIdB64)) }
        entries = []
        states = [:]
        chat = nil
        accountIdB64 = nil
    }
}

/// ADR-0092 section 3.6 — the account's own choices in polls, from the
/// DeviceGroup synced register, plus what this device chose and has not
/// recorded yet.
@MainActor
public final class PollChoiceCenter: ObservableObject {
    public static let shared = PollChoiceCenter()

    @Published public private(set) var revision: UInt64 = 0

    private weak var sync: DeviceGroupSyncRepository?
    private weak var chat: ChatRepository?
    private var local: [String: [UInt8]] = [:]
    private var watch: AnyCancellable?

    public func bind(sync: DeviceGroupSyncRepository, chat: ChatRepository) {
        self.sync = sync
        self.chat = chat
        watch = sync.$pollChoiceRevision.sink { [weak self] _ in self?.revision &+= 1 }
    }

    /// The account's choice in a group poll; `nil` when it has not voted.
    public func choice(groupIdHex: String, pollMessageId: Int64) -> [UInt8]? {
        if let mine = local["\(groupIdHex):\(pollMessageId)"] { return mine }
        guard let ref = chat?.folderRef(groupIdHex: groupIdHex),
              let entry = sync?.pollChoice(conversation: ref, pollMessageId: pollMessageId) else { return nil }
        return [UInt8](entry.choices)
    }

    public func setVersion(groupIdHex: String, pollMessageId: Int64) -> UInt32? {
        guard let ref = chat?.folderRef(groupIdHex: groupIdHex) else { return nil }
        return sync?.pollChoice(conversation: ref, pollMessageId: pollMessageId)?.setVersion
    }

    /// Show a vote as cast at once (the ballot is queued).
    func noteLocal(groupIdHex: String, pollMessageId: Int64, choices: [UInt8]) {
        local["\(groupIdHex):\(pollMessageId)"] = choices
        revision &+= 1
    }

    /// Record the choice in the synced register after its ballot was posted.
    func record(groupIdHex: String, pollMessageId: Int64, choices: [UInt8], setVersion: UInt32) async {
        guard let ref = chat?.folderRef(groupIdHex: groupIdHex) else { return }
        _ = await sync?.setPollChoice(
            conversation: ref, pollMessageId: pollMessageId, choices: choices, setVersion: setVersion)
        revision &+= 1
    }

    public func reset() {
        local = [:]
        sync = nil
        chat = nil
        watch = nil
    }
}

// MARK: - ADR-0092 section 3.9: the open-items hand-over

extension ChatRepository {

    /// Committer side: after a merged admission commit that added `addedLeaves`
    /// leaves, hand every open poll and checklist (and the old ballot rows of
    /// open anonymous polls) to the group, sealed at the new epoch. Spawns a
    /// task: the caller holds the commit lock the publish takes.
    func scheduleContentHandover(groupIdHex: String, addedLeaves: UInt32) {
        guard addedLeaves > 0, let record = groups[groupIdHex] else { return }
        let plan = Self.handoverPlan(messages: record.messages, nowMs: Self.nowMs())
        guard openItemsHandoverDue(addedLeaves: addedLeaves, heldItems: UInt32(plan.items.count)) else { return }
        let rows: [Data] = ((try? planOpenItemsRows(items: plan.items)) ?? []) + plan.ballotRows
        guard !rows.isEmpty else { return }
        Task { @MainActor [weak self] in
            for row in rows {
                do {
                    try await self?.publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: row)
                } catch {
                    if !ConversationReentryRules.isBehindRefusal(error) { NSLog("ADR-0092: open-items hand-over row not sent") }
                    return
                }
            }
        }
    }

    /// The open items of `record` and the old ballot rows to hand over.
    static func handoverPlan(messages: [ChatMessage], nowMs: Int64) -> (items: [OpenItem], ballotRows: [Data]) {
        var items: [OpenItem] = []
        var ballotRows: [Data] = []
        for m in messages where m.messageId > 0 && !m.deleted {
            guard let stored = m.content, let content = ContentModel.content(stored),
                  let author = m.authorKeyB64.flatMap({ Data(base64Encoded: $0) }) else { continue }
            switch content {
            case .poll(let poll):
                guard let view = PollView.make(message: m, ownAccountKey: nil, registerChoice: nil, nowMs: nowMs),
                      !view.closed, !view.unverifiable else { continue }
                let ring = stored.ops.ringB64.flatMap { Data(base64Encoded: $0) }
                    .flatMap { try? PollRing.decode(bytes: $0) }
                if let item = try? openItemForPoll(
                    pollRef: m.messageId, poll: poll, author: author, ring: ring,
                    votes: ContentModel.voteRecords(stored.ops)) {
                    items.append(item)
                }
                if poll.body.anonymous != nil {
                    let old = stored.ops.ballots
                        .filter { ballotNeedsHandover(createdAtMs: $0.createdAtMs, nowMs: nowMs) }
                        .compactMap { Data(base64Encoded: $0.rowB64) }
                    if !old.isEmpty { ballotRows += planPollBallotsRows(pollRef: m.messageId, rows: old) }
                }
            case .checklist(let original):
                guard let view = ChecklistView.make(message: m), !view.closed else { continue }
                // The current form: the author's latest edit, else the original.
                let current = stored.ops.edits.last
                    .flatMap { Data(base64Encoded: $0.envelopeB64) }
                    .flatMap { bytes -> SignedChecklist? in
                        if case .checklist(let signed)? = parseBodyEnvelope(bytes: bytes).content { return signed }
                        return nil
                    } ?? original
                if let item = try? openItemForChecklist(
                    pollRef: m.messageId, current: current, author: author, state: view.state,
                    ticks: ContentModel.tickRecords(stored.ops)) {
                    items.append(item)
                }
            default:
                continue
            }
        }
        return (items, ballotRows)
    }

    /// Receiver side: judge the sender (owner or admin of the live group),
    /// assemble the rows and adopt every item this device does not hold.
    func receiveContentHandover(groupIdHex: String, event: IncomingChatEvent) {
        guard let record = groups[groupIdHex] else { return }
        switch event {
        case .openItems(_, let envelope, let senderKey, let createdAt):
            guard let senderKey, let sender = Data(base64Encoded: senderKey),
                  senderKey != AuthorIdentity.own(group: record.group),
                  // Taken from an owner or admin, or from the account that
                  // committed this device's admission (the Welcome's sender,
                  // recorded at the join through `peekWelcomeFounder`).
                  ((try? openItemsSenderTrusted(
                    senderAccountKey: sender,
                    admittingAccountKey: currentAccountIdB64.flatMap {
                        ContentAdmitters.load(accountIdB64: $0, groupIdHex: groupIdHex)
                    },
                    senderIsOwnerOrAdmin: record.isOwnerOrAdmin(sender))) ?? false) else { return }
            ContentHandoverInbox.rows[record.groupId, default: []].append(
                ReceivedOpenItemsRow(senderAccountKey: sender, envelope: envelope))
            // A row restored from a history bundle (a snapshot, no ops of its
            // own) is NOT held: the hand-over replaces it, so its counts are
            // never added to what the hand-over carries.
            let held = record.messages.filter { m in
                m.messageId > 0 && !(m.content.map { $0.state != nil && $0.ops == PersistedContentOps() } ?? false)
            }.map(\.messageId)
            guard let assembled = try? assembleOpenItems(
                rows: ContentHandoverInbox.rows[record.groupId] ?? [], held: held) else { return }
            var adopted = false
            for item in assembled {
                guard let open = try? adoptOpenItem(groupId: record.groupId, item: item) else { continue }
                adopted = adoptHandedOver(record: record, item: open, createdAt: createdAt) || adopted
            }
            if adopted {
                ContentHandoverInbox.rows[record.groupId] = nil
                backfillBallots(groupIdHex: groupIdHex)
                persistMessageLogBestEffort()
                state = state.with(threads: buildThreadList())
            }
        case .pollBallots(_, let envelope, let senderKey, _):
            guard let senderKey, let sender = Data(base64Encoded: senderKey), record.isOwnerOrAdmin(sender),
                  let decoded = try? decodePollBallotsEnvelope(bytes: envelope),
                  let poll = record.messages.first(where: { $0.messageId == decoded.pollRef }),
                  poll.content != nil else { return }
            // Old ballots carry no DS id of their own here: they are filed
            // below every live row (negative ids, in the order handed over),
            // which the fold reads as earlier than any close.
            var changed = false
            for (i, row) in decoded.rows.enumerated() {
                let synthetic = -Int64(i + 1)
                if record.openBallot(messageId: synthetic, row: row, createdAtMs: 0) { changed = true }
            }
            if changed { persistMessageLogBestEffort() }
        default:
            return
        }
    }

    private func adoptHandedOver(record: GroupRecord, item: AdoptedOpenItem, createdAt: String) -> Bool {
        switch item {
        case .poll(let pollRef, let author, let poll, let ring, let votes):
            guard !Self.holdsLive(record, pollRef),
                  var stored = ContentModel.store(content: .poll(poll: poll), attachments: []) else { return false }
            stored.ops.ringB64 = (try? ring?.encode())??.base64EncodedString()
            stored.ops.votes = votes.map {
                .init(messageId: $0.messageId, createdAtMs: $0.createdAtMs, voterB64: $0.voter.base64EncodedString(),
                      choices: [UInt8]($0.choices))
            }
            record.insertHandedOver(ChatMessage(
                messageId: pollRef, direction: .incoming, text: "", createdAt: createdAt,
                authorKeyB64: author.base64EncodedString(), content: stored))
            return true
        case .checklist(let pollRef, let author, let checklist, let adds, let ticks):
            guard !Self.holdsLive(record, pollRef),
                  var stored = ContentModel.store(content: .checklist(checklist: checklist), attachments: [])
            else { return false }
            stored.ops.adds = adds.map {
                .init(messageId: $0.messageId, adderB64: $0.adder.base64EncodedString(), privileged: true, text: $0.text)
            }
            stored.ops.ticks = ticks.map {
                .init(messageId: $0.messageId, tickerB64: $0.ticker.base64EncodedString(), privileged: true,
                      item: ContentModel.persisted($0.item), done: $0.done)
            }
            record.insertHandedOver(ChatMessage(
                messageId: pollRef, direction: .incoming, text: "", createdAt: createdAt,
                authorKeyB64: author.base64EncodedString(), content: stored))
            return true
        }
    }
}

extension ChatRepository {
    /// Whether `record` holds `pollRef` as a live row (not a restored
    /// snapshot that a hand-over replaces).
    static func holdsLive(_ record: GroupRecord, _ pollRef: Int64) -> Bool {
        guard let m = record.messages.first(where: { $0.messageId == pollRef }) else { return false }
        return !(m.content.map { $0.state != nil && $0.ops == PersistedContentOps() } ?? false)
    }

    /// ADR-0092 section 3.9 — the ballots of an open anonymous poll that
    /// were posted before this device's admission are rows on the journal it
    /// never read (its cursor starts at the join): fetch the journal from the
    /// oldest open anonymous poll onward and file only its ballot rows. The
    /// cursor does not move; MLS rows are not touched.
    func backfillBallots(groupIdHex: String) {
        guard let record = groups[groupIdHex], let identity = canonicalIdentity else { return }
        let polls = record.messages.filter { m in
            guard m.messageId > 0, let stored = m.content, stored.ops.ringB64 != nil else { return false }
            return true
        }
        guard let from = polls.map(\.messageId).min() else { return }
        let client = chatClient
        Task { @MainActor [weak self] in
            var after = from
            for _ in 0..<50 {
                guard let live = self?.groups[groupIdHex] else { return }
                let auth = await client.dsAuth(identity: identity, group: live.group, groupId: live.groupId)
                guard let page = try? await client.fetchGroupJournal(
                    groupId: live.groupId, after: after, waitSecs: 0, auth: auth) else { return }
                var changed = false
                for row in page.messages where isPollBallotRow(row: row.payload) {
                    if live.openBallot(messageId: row.id, row: row.payload, createdAtMs: ContentTime.ms(row.createdAt)) {
                        changed = true
                    }
                }
                if changed {
                    self?.persistMessageLogBestEffort()
                    self?.objectWillChangeContent()
                }
                guard page.hasMore, let last = page.messages.last?.id, last > after else { return }
                after = last
            }
        }
    }
}

/// ADR-0092 section 3.9 — the account that committed this device's admission
/// to each group, per account.
enum ContentAdmitters {
    static func key(_ account: String) -> String { "lakko.content-admitters.v1.\(account)" }

    static func save(accountIdB64: String, groupIdHex: String, key admitter: Data) {
        var map = UserDefaults.standard.dictionary(forKey: key(accountIdB64)) as? [String: String] ?? [:]
        map[groupIdHex] = admitter.base64EncodedString()
        UserDefaults.standard.set(map, forKey: key(accountIdB64))
    }

    static func load(accountIdB64: String, groupIdHex: String) -> Data? {
        (UserDefaults.standard.dictionary(forKey: key(accountIdB64)) as? [String: String])?[groupIdHex]
            .flatMap { Data(base64Encoded: $0) }
    }
}

/// Hand-over rows received and not yet assembled (a ring split over several
/// rows). In memory.
@MainActor
enum ContentHandoverInbox {
    static var rows: [Data: [ReceivedOpenItemsRow]] = [:]
}
