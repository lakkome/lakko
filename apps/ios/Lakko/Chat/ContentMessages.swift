// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0090 — polls, checklists, locations, contact cards and articles in a
// group conversation: how they are stored with their message, how the ops on
// them are folded (always by the core: `foldPoll`, `foldChecklist`,
// `foldPollBallots`), and how the receive path files each event.
//
// Every rule that decides a tally or a tick lives in Rust; this file only
// gathers the ops a message received and hands them over.

/// ADR-0090 section 3.4 — ring version 0 of an anonymous poll, as checked at
/// receipt.
public enum ContentRingCheck: Equatable {
    /// The encoded ring (`PollRing.encode()`), version 0 matched the poll's
    /// signed `ring0_hash`.
    case ring(Data)
    /// The roster this device read did not match: the poll shows no totals
    /// and takes no vote (fail-closed).
    case mismatch
}

/// ADR-0090 section 3.4 — the ring from a group's roster.
enum ContentRing {
    /// The distinct account keys of `group`'s roster, sorted bytewise.
    static func accountKeys(of group: Group) -> [Data] {
        let keys = AuthorIdentity.roster(of: group).map(\.signaturePublicKey)
        return sortedDistinct(keys)
    }

    /// Whether a failed content message may be sent again as it is (the
    /// mirror of web `contentRetryAllowed`): anything but an anonymous poll
    /// may; an anonymous poll only while the hash of the roster's distinct,
    /// sorted account keys still equals the first-ring hash it signed.
    static func retryAllowed(content: MessageContent?, rosterKeys: [Data]) -> Bool {
        guard case .poll(let poll)? = content, let anonymous = poll.body.anonymous else { return true }
        let keys = sortedDistinct(rosterKeys)
        guard !keys.isEmpty, let hash = try? pollRingHash(keys: keys) else { return false }
        return hash == anonymous.ring0Hash
    }

    static func sortedDistinct(_ keys: [Data]) -> [Data] {
        Array(Set(keys)).sorted { $0.lexicographicallyPrecedes($1) }
    }

    /// Version 0 of an anonymous poll's ring, read from the roster NOW (the
    /// caller is on the line after the poll's apply).
    static func ringAtReceipt(poll: SignedPoll, group: Group) -> ContentRingCheck {
        ring0(poll: poll, keys: accountKeys(of: group))
    }

    static func ring0(poll: SignedPoll, keys: [Data]) -> ContentRingCheck {
        guard let anonymous = poll.body.anonymous, !keys.isEmpty,
              let hash = try? pollRingHash(keys: keys), hash == anonymous.ring0Hash,
              let ring = try? PollRing(keys: keys), let encoded = try? ring.encode() else {
            return .mismatch
        }
        return .ring(encoded)
    }
}

// MARK: - Reading a stored content message

/// ADR-0090 — the read side of a stored content message.
enum ContentModel {

    /// The parsed envelope a content message is stored as.
    static func parsed(_ stored: PersistedContent) -> ParsedBody? {
        guard let bytes = Data(base64Encoded: stored.envelopeB64) else { return nil }
        let parsed = parseBodyEnvelope(bytes: bytes)
        return parsed.content == nil ? nil : parsed
    }

    /// The content of a stored message.
    static func content(_ stored: PersistedContent) -> MessageContent? {
        parsed(stored)?.content
    }

    /// Store `content` (with the message's attachments, which an article's
    /// images index) as an envelope the core composes.
    static func store(
        content: MessageContent, attachments: [AttachmentRef], parts: ContentEnvelopeParts = emptyParts
    ) -> PersistedContent? {
        guard let bytes = try? composeContentEnvelope(
            content: content, attachments: attachments, parts: parts) else { return nil }
        return PersistedContent(envelopeB64: bytes.base64EncodedString())
    }

    static let emptyParts = ContentEnvelopeParts(
        replyTo: nil, replyAuthor: nil, replyExcerpt: nil, forwardedFrom: nil,
        forwardOriginGroupB64: nil, forwardOriginMessageId: nil)

    /// The parts of a forwarded content message: the forward line and, when
    /// the forward names one, its origin (standard base64 of the group id, the
    /// wire shape). `emptyParts` for no forward.
    static func forwardParts(_ forward: ForwardRef?) -> ContentEnvelopeParts {
        guard let forward else { return emptyParts }
        let originB64 = forward.originGroupIdHex.flatMap(ForwardPayload.groupIdB64(fromHex:))
        return ContentEnvelopeParts(
            replyTo: nil, replyAuthor: nil, replyExcerpt: nil, forwardedFrom: forward.author,
            forwardOriginGroupB64: originB64,
            forwardOriginMessageId: originB64 == nil ? nil : forward.originMessageId)
    }

    // MARK: Record conversions

    static func itemRef(_ p: PersistedItemRef) -> ChecklistItemRef? {
        if let id = p.authorId { return .author(id: id) }
        if let mid = p.addedMessageId { return .added(messageId: mid) }
        return nil
    }

    static func persisted(_ ref: ChecklistItemRef) -> PersistedItemRef {
        switch ref {
        case .author(let id): return PersistedItemRef(authorId: id)
        case .added(let mid): return PersistedItemRef(addedMessageId: mid)
        }
    }

    static func key(_ b64: String) -> Data { Data(base64Encoded: b64) ?? Data() }

    static func voteRecords(_ ops: PersistedContentOps) -> [PollVoteRecord] {
        ops.votes.map {
            PollVoteRecord(
                messageId: $0.messageId, createdAtMs: $0.createdAtMs, voter: key($0.voterB64),
                choices: Data($0.choices))
        }
    }

    static func closeRecords(_ ops: PersistedContentOps) -> [PollCloseRecord] {
        ops.closes.map {
            PollCloseRecord(messageId: $0.messageId, closer: key($0.closerB64), closerIsAdmin: $0.closerIsAdmin)
        }
    }

    static func tickRecords(_ ops: PersistedContentOps) -> [ChecklistTickRecord] {
        ops.ticks.compactMap { t in
            itemRef(t.item).map {
                ChecklistTickRecord(
                    messageId: t.messageId, ticker: key(t.tickerB64), privileged: t.privileged,
                    item: $0, done: t.done)
            }
        }
    }

    static func addRecords(_ ops: PersistedContentOps) -> [ChecklistAddRecord] {
        ops.adds.map {
            ChecklistAddRecord(
                messageId: $0.messageId, adder: key($0.adderB64), privileged: $0.privileged, text: $0.text)
        }
    }

    static func editRecords(_ ops: PersistedContentOps) -> [ChecklistEditRecord] {
        ops.edits.compactMap { e in
            guard let bytes = Data(base64Encoded: e.envelopeB64),
                  case .checklist(let signed)? = parseBodyEnvelope(bytes: bytes).content else { return nil }
            return ChecklistEditRecord(messageId: e.messageId, editor: key(e.editorB64), body: signed.body)
        }
    }

    // MARK: State snapshots (history bundle, vault)

    static func persisted(_ state: ContentState) -> PersistedContentState {
        switch state {
        case .poll(let counts, let voters, let closed):
            let (kind, by): (String, Int64?) = {
                switch closed {
                case .open: return ("open", nil)
                case .byTime: return ("time", nil)
                case .byOp(let id): return ("op", id)
                }
            }()
            return PersistedContentState(kind: "poll", counts: counts, voters: voters, closed: kind, closedBy: by)
        case .checklist(let closedBy, let added, let done):
            return PersistedContentState(
                kind: "checklist", closedBy: closedBy,
                added: added.map { PersistedAddedItem(messageId: $0.messageId, text: $0.text) },
                done: done.map { PersistedDoneItem(item: persisted($0.item), byB64: $0.by.base64EncodedString()) })
        }
    }

    static func contentState(_ p: PersistedContentState) -> ContentState? {
        switch p.kind {
        case "poll":
            let closed: PollClosedState
            switch p.closed {
            case "time": closed = .byTime
            case "op": closed = .byOp(messageId: p.closedBy ?? 0)
            default: closed = .open
            }
            return .poll(counts: p.counts ?? [], voters: p.voters ?? 0, closed: closed)
        case "checklist":
            return .checklist(
                closedBy: p.closedBy,
                added: (p.added ?? []).map { AddedChecklistItem(messageId: $0.messageId, text: $0.text) },
                done: (p.done ?? []).compactMap { d in
                    itemRef(d.item).map { DoneChecklistItem(item: $0, by: key(d.byB64)) }
                })
        default:
            return nil
        }
    }

    /// The `ContentMessage` a history bundle carries for `message`.
    static func bundleContent(_ message: ChatMessage, nowMs: Int64) -> ContentMessage? {
        guard let stored = message.content, let content = content(stored) else { return nil }
        var state: ContentState?
        switch content {
        case .poll:
            if let view = PollView.make(message: message, ownAccountKey: nil, registerChoice: nil, nowMs: nowMs) {
                state = .poll(
                    counts: view.counts, voters: view.voters,
                    closed: view.closedBy.map { .byOp(messageId: $0) } ?? (view.closed ? .byTime : .open))
            }
        case .checklist:
            if let view = ChecklistView.make(message: message), let snap = try? checklistStateSnapshot(state: view.state) {
                state = snap
            }
        default:
            break
        }
        return ContentMessage(content: content, state: state ?? stored.state.flatMap(contentState))
    }

    /// A row restored from a history bundle or the vault.
    static func restored(_ content: ContentMessage, attachments: [AttachmentRef]) -> PersistedContent? {
        guard var stored = store(content: content.content, attachments: attachments) else { return nil }
        stored.state = content.state.map(persisted)
        return stored
    }
}

// MARK: - Poll view model

/// ADR-0090 section 3 / UX section 7.2 — what a poll card shows.
struct PollView: Equatable {
    let poll: SignedPoll
    let counts: [UInt32]
    let voters: UInt32
    let closed: Bool
    /// DS id of the close op, when closed by one.
    let closedBy: Int64?
    /// This account's choice; `nil` when it has not voted (or retracted).
    let ownChoices: [UInt8]?
    /// The ring did not check: no totals, no voting.
    let unverifiable: Bool
    /// For a non-anonymous poll: who chose what (account keys).
    let choicesByVoter: [PollVoterChoices]
    /// A channel poll's frozen tally, set by a close.
    let frozen: Bool

    var anonymous: Bool { poll.body.anonymous != nil }
    var quiz: PollQuiz? { poll.body.quiz }

    /// Fold `message`'s poll. `registerChoice` is the account's own choice
    /// from the synced register (anonymous and channel polls); `nil` when
    /// none is recorded.
    static func make(
        message: ChatMessage, ownAccountKey: Data?, registerChoice: [UInt8]?, nowMs: Int64
    ) -> PollView? {
        guard let stored = message.content, case .poll(let poll)? = ContentModel.content(stored) else { return nil }
        let ops = stored.ops
        let author = message.authorKeyB64.flatMap { Data(base64Encoded: $0) } ?? Data()
        // A history-bundle snapshot with no ops since: show it as carried.
        if ops.votes.isEmpty, ops.ballots.isEmpty, ops.closes.isEmpty,
           let snap = stored.state, snap.kind == "poll" {
            let closed = snap.closed == "op" || snap.closed == "time"
                || (poll.body.closeAtMs.map { nowMs >= $0 } ?? false)
            return PollView(
                poll: poll, counts: snap.counts ?? Array(repeating: 0, count: poll.body.options.count),
                voters: snap.voters ?? 0, closed: closed, closedBy: snap.closed == "op" ? snap.closedBy : nil,
                ownChoices: registerChoice, unverifiable: false, choicesByVoter: [], frozen: false)
        }
        if poll.body.anonymous == nil {
            guard let tally = try? foldPoll(
                body: poll.body, author: author, pollMessageId: message.messageId,
                votes: ContentModel.voteRecords(ops), closes: ContentModel.closeRecords(ops)) else { return nil }
            let own = ownAccountKey.flatMap { key in
                tally.choicesByVoter.first { $0.voter == key }.map { [UInt8]($0.choices) }
            }
            return PollView(
                poll: poll, counts: tally.counts, voters: tally.voters,
                closed: pollTallyIsClosed(tally: tally, nowMs: nowMs), closedBy: tally.closedBy,
                ownChoices: own.flatMap { $0.isEmpty ? nil : $0 } ?? registerChoice.flatMap { $0.isEmpty ? nil : $0 },
                unverifiable: false, choicesByVoter: tally.choicesByVoter, frozen: false)
        }
        // Anonymous: the ballots this device verified, folded by the core.
        let closedBy = validClose(ops: ops, author: author)
        let rules = PollBallotRules(
            optionCount: UInt8(clamping: poll.body.options.count), multiple: poll.body.multiple,
            closeMessageId: closedBy, closeAtMs: poll.body.closeAtMs)
        let ballots = ops.ballots.compactMap { b -> CountedPollBallot? in
            guard let tag = b.tagB64.flatMap({ Data(base64Encoded: $0) }), let choices = b.choices else { return nil }
            return CountedPollBallot(
                messageId: b.messageId, createdAtMs: b.createdAtMs, tag: tag, choices: Data(choices))
        }
        let tally = (try? foldPollBallots(rules: rules, ballots: ballots))
            ?? PollBallotTally(counts: Array(repeating: 0, count: poll.body.options.count), voters: 0)
        let closed = closedBy != nil || (poll.body.closeAtMs.map { nowMs >= $0 } ?? false)
        return PollView(
            poll: poll, counts: tally.counts, voters: tally.voters, closed: closed, closedBy: closedBy,
            ownChoices: registerChoice.flatMap { $0.isEmpty ? nil : $0 },
            unverifiable: ops.unverifiable, choicesByVoter: [], frozen: false)
    }

    /// The first close by the poll's author or an owner / admin at receipt.
    static func validClose(ops: PersistedContentOps, author: Data) -> Int64? {
        ops.closes
            .filter { $0.closerIsAdmin || ContentModel.key($0.closerB64) == author }
            .map(\.messageId)
            .min()
    }

    /// The share of `index` in percent, rounded.
    func percent(_ index: Int) -> Int {
        guard voters > 0, index < counts.count else { return 0 }
        return Int((Double(counts[index]) * 100 / Double(voters)).rounded())
    }

    /// Whether the person sees the results (voted, or closed).
    var showsResults: Bool { closed || ownChoices != nil }
}

// MARK: - Checklist view model

/// ADR-0090 section 5 / UX section 8.2 — what a checklist card shows.
struct ChecklistView: Equatable {
    let signed: SignedChecklist
    let state: ChecklistState
    let author: Data

    var closed: Bool { state.closedBy != nil }
    var doneCount: Int { state.items.filter { $0.doneBy != nil }.count }

    static func make(message: ChatMessage) -> ChecklistView? {
        guard let stored = message.content, case .checklist(let signed)? = ContentModel.content(stored) else {
            return nil
        }
        let author = message.authorKeyB64.flatMap { Data(base64Encoded: $0) } ?? Data()
        let ops = stored.ops
        let closes = ops.closes.map { ChecklistCloseRecord(messageId: $0.messageId, closer: ContentModel.key($0.closerB64)) }
        var adds = ContentModel.addRecords(ops)
        var ticks = ContentModel.tickRecords(ops)
        // A history-bundle snapshot: its added and done items, as privileged
        // records (the sending device's fold already applied the flags).
        if let snap = stored.state, snap.kind == "checklist" {
            for a in snap.added ?? [] where !adds.contains(where: { $0.messageId == a.messageId }) {
                adds.append(ChecklistAddRecord(messageId: a.messageId, adder: author, privileged: true, text: a.text))
            }
            for d in snap.done ?? [] {
                guard let ref = ContentModel.itemRef(d.item) else { continue }
                ticks.insert(
                    ChecklistTickRecord(
                        messageId: 0, ticker: ContentModel.key(d.byB64), privileged: true, item: ref, done: true),
                    at: 0)
            }
        }
        guard let state = try? foldChecklist(
            body: signed.body, author: author, checklistMessageId: message.messageId,
            edits: ContentModel.editRecords(ops), adds: adds, ticks: ticks,
            closes: closes) else { return nil }
        return ChecklistView(signed: signed, state: state, author: author)
    }

    /// Whether `who` may tick an item (the author always; others when the
    /// flag is set), and add one.
    func mayTick(_ who: Data?) -> Bool {
        guard !closed, let who else { return false }
        return who == author || state.othersMark
    }

    func mayAdd(_ who: Data?) -> Bool {
        guard !closed, let who else { return false }
        return who == author || state.othersAdd
    }
}

// MARK: - Receive side

extension GroupRecord {

    /// Whether `accountKey` is the owner or an admin of this group's live role
    /// table (the ADR-0022 committer rank exists exactly for those).
    func isOwnerOrAdmin(_ accountKey: Data) -> Bool {
        ((try? group.committerRank(signatureKey: accountKey)) ?? nil) != nil
    }

    /// ADR-0090 — file one content event. Returns `true` when the timeline
    /// changed.
    @discardableResult
    func applyContentEvent(_ event: IncomingChatEvent, ownAuthorKey: MemberIdentity?) -> Bool {
        switch event {
        case .content(let messageId, let parsed, let envelope, let senderKey, let createdAt, let ring):
            return applyContentRow(
                messageId: messageId, parsed: parsed, envelope: envelope, senderKey: senderKey,
                createdAt: createdAt, ring: ring, ownAuthorKey: ownAuthorKey)
        case .contentOp(let messageId, let parsed, let envelope, let senderKey, let createdAt):
            return applyContentOp(
                messageId: messageId, parsed: parsed, envelope: envelope, senderKey: senderKey, createdAt: createdAt)
        case .pollBallotRow(let messageId, let row, let createdAt):
            return applyBallotRow(messageId: messageId, row: row, createdAtMs: ContentTime.ms(createdAt))
        case .commitRoster(_, let keys):
            return extendRings(with: keys)
        default:
            return false
        }
    }

    private func applyContentRow(
        messageId: Int64, parsed: ParsedBody, envelope: Data, senderKey: MemberIdentity?, createdAt: String,
        ring: ContentRingCheck?, ownAuthorKey: MemberIdentity?
    ) -> Bool {
        guard let content = parsed.content, let senderKey, let author = Data(base64Encoded: senderKey) else {
            return false
        }
        // The author signature binds a poll or checklist to its author and
        // this group (ADR-0090 section 3.1); one that fails is dropped.
        switch content {
        case .poll(let poll):
            guard (try? verifyPoll(poll: poll, groupId: groupId, authorKey: author)) != nil else { return false }
            // An anonymous poll is refused in a one-to-one by every receiver.
            if poll.body.anonymous != nil, (try? group.groupKind()) == .oneToOne { return false }
        case .checklist(let checklist):
            guard (try? verifyChecklist(checklist: checklist, groupId: groupId, authorKey: author)) != nil else {
                return false
            }
        default:
            break
        }
        var stored = PersistedContent(envelopeB64: envelope.base64EncodedString())
        switch ring {
        case .ring(let encoded)?: stored.ops.ringB64 = encoded.base64EncodedString()
        case .mismatch?: stored.ops.unverifiable = true
        case nil: break
        }
        let fromOwnDevice = senderKey == ownAuthorKey
        appendContentRow(
            ChatMessage(
                messageId: messageId, direction: fromOwnDevice ? .outgoing : .incoming, text: "",
                attachments: parsed.attachments, createdAt: createdAt, authorKeyB64: senderKey,
                reply: parsed.replyTo.map {
                    ReplyRef(target: $0, author: parsed.replyAuthor, excerpt: parsed.replyExcerpt)
                },
                forward: ForwardPayload.received(
                    author: parsed.forwardedFrom, originGroupIdB64: parsed.forwardOriginGroupB64,
                    originMessageId: parsed.forwardOriginMessageId),
                content: stored))
        // Ballots that arrived ahead of a poll this device just learned.
        retryUnmatchedBallots()
        return true
    }

    private func applyContentOp(
        messageId: Int64, parsed: ParsedBody, envelope: Data, senderKey: MemberIdentity?, createdAt: String
    ) -> Bool {
        guard let target = parsed.target, let senderKey, let sender = Data(base64Encoded: senderKey) else {
            return false
        }
        let createdAtMs = ContentTime.ms(createdAt)
        let isAdmin = isOwnerOrAdmin(sender)
        return mutateMessage(messageId: target) { m in
            guard var stored = m.content, !m.deleted else { return }
            switch parsed.kind {
            case .pollVote:
                guard let choices = parsed.pollChoices else { return }
                stored.ops.votes.append(.init(
                    messageId: messageId, createdAtMs: createdAtMs, voterB64: senderKey, choices: [UInt8](choices)))
            case .pollClose:
                guard !stored.ops.closes.contains(where: { $0.messageId == messageId }) else { return }
                stored.ops.closes.append(.init(messageId: messageId, closerB64: senderKey, closerIsAdmin: isAdmin))
            case .checklistTick:
                guard let item = parsed.checklistItem, let done = parsed.checklistDone else { return }
                stored.ops.ticks.append(.init(
                    messageId: messageId, tickerB64: senderKey, privileged: false,
                    item: ContentModel.persisted(item), done: done))
            case .checklistAdd:
                stored.ops.adds.append(.init(messageId: messageId, adderB64: senderKey, privileged: false, text: parsed.text))
            case .editChecklist:
                stored.ops.edits.append(.init(
                    messageId: messageId, editorB64: senderKey, envelopeB64: envelope.base64EncodedString()))
            case .editArticle:
                // Only the author replaces an article (the edit gate).
                guard m.authorKeyB64 == senderKey, case .article(let article)? = parsed.content,
                      let replaced = ContentModel.store(content: .article(article: article), attachments: parsed.attachments)
                else { return }
                stored.envelopeB64 = replaced.envelopeB64
                m.attachments = parsed.attachments
                m.edited = true
                m.noteOp(messageId)
            default:
                return
            }
            m.content = stored
        }
    }

    // MARK: Anonymous ballots

    /// The web client's rule: the poll whose secret opens the row owns it.
    /// The row is filed on that poll and persisted with the message log; one
    /// whose ring version this device does not hold yet is kept unverified and
    /// verified again after the next commit. A row no held poll opens is
    /// dropped (a newcomer gets it from the hand-over or the journal
    /// back-fill).
    private func applyBallotRow(messageId: Int64, row: Data, createdAtMs: Int64) -> Bool {
        openBallot(messageId: messageId, row: row, createdAtMs: createdAtMs)
    }

    /// File `row` on the anonymous poll it opens under; `true` when filed.
    @discardableResult
    func openBallot(messageId: Int64, row: Data, createdAtMs: Int64) -> Bool {
        let rowB64 = row.base64EncodedString()
        for m in messages where m.messageId > 0 && (messageId < 0 || m.messageId < messageId) {
            guard let stored = m.content, !m.deleted,
                  case .poll(let poll)? = ContentModel.content(stored), let anon = poll.body.anonymous,
                  let keys = try? PollBallotKeys(groupId: groupId, pollId: poll.body.id, secret: anon.secret),
                  (try? openPollBallotRow(keys: keys, row: row)) != nil else { continue }
            // Idempotent by DS id and by row (a back-filled row and the same
            // row from a hand-over are one ballot).
            if stored.ops.ballots.contains(where: { $0.messageId == messageId || $0.rowB64 == rowB64 }) { return true }
            let verified = Self.verify(stored: stored, keys: keys, row: row)
            mutateMessage(messageId: m.messageId) { msg in
                msg.content?.ops.ballots.append(.init(
                    messageId: messageId, createdAtMs: createdAtMs, rowB64: rowB64,
                    tagB64: verified?.tag.base64EncodedString(), choices: verified.map { [UInt8]($0.choices) },
                    version: verified?.version))
            }
            return true
        }
        return false
    }

    private static func verify(stored: PersistedContent, keys: PollBallotKeys, row: Data) -> VerifiedPollBallot? {
        guard let ringBytes = stored.ops.ringB64.flatMap({ Data(base64Encoded: $0) }),
              let ring = try? PollRing.decode(bytes: ringBytes) else { return nil }
        return try? openAndVerifyPollBallot(keys: keys, ring: ring, row: row)
    }

    /// Verify again every filed ballot still unverified (a ring version that
    /// arrived with a commit since).
    func retryUnmatchedBallots() {
        for m in messages where m.messageId > 0 && !m.deleted {
            guard let stored = m.content, stored.ops.ballots.contains(where: { $0.tagB64 == nil }),
                  case .poll(let poll)? = ContentModel.content(stored), let anon = poll.body.anonymous,
                  let keys = try? PollBallotKeys(groupId: groupId, pollId: poll.body.id, secret: anon.secret)
            else { continue }
            var ops = stored.ops
            var changed = false
            for i in ops.ballots.indices where ops.ballots[i].tagB64 == nil {
                guard let row = Data(base64Encoded: ops.ballots[i].rowB64),
                      let v = Self.verify(stored: stored, keys: keys, row: row) else { continue }
                ops.ballots[i].tagB64 = v.tag.base64EncodedString()
                ops.ballots[i].choices = [UInt8](v.choices)
                ops.ballots[i].version = v.version
                changed = true
            }
            if changed { mutateMessage(messageId: m.messageId) { $0.content?.ops = ops } }
        }
    }

    /// ADR-0090 section 3.4 — append the accounts a merged commit added to the
    /// ring of every open anonymous poll.
    private func extendRings(with keys: [Data]) -> Bool {
        var changed = false
        let now = ChatRepository.nowMs()
        for m in messages where m.messageId > 0 && !m.deleted {
            guard let stored = m.content, let ringB64 = stored.ops.ringB64,
                  let bytes = Data(base64Encoded: ringB64), let ring = try? PollRing.decode(bytes: bytes),
                  let view = PollView.make(message: m, ownAccountKey: nil, registerChoice: nil, nowMs: now),
                  !view.closed, ((try? ring.extend(added: keys)) ?? nil) != nil,
                  let encoded = try? ring.encode() else { continue }
            mutateMessage(messageId: m.messageId) { $0.content?.ops.ringB64 = encoded.base64EncodedString() }
            changed = true
        }
        retryUnmatchedBallots()
        return changed
    }
}

/// DS `created_at` (ISO 8601) in milliseconds.
enum ContentTime {
    static func ms(_ iso: String) -> Int64 {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = f.date(from: iso) { return Int64(d.timeIntervalSince1970 * 1000) }
        f.formatOptions = [.withInternetDateTime]
        if let d = f.date(from: iso) { return Int64(d.timeIntervalSince1970 * 1000) }
        return 0
    }
}

/// ADR-0090 section 1 — the one-line preview of a content message (the
/// conversation list; the notification extension builds the same line).
enum ContentPreview {
    static func line(_ message: ChatMessage) -> String? {
        guard !message.deleted, let stored = message.content, let content = ContentModel.content(stored) else {
            return nil
        }
        return line(content: content)
    }

    static func line(content: MessageContent?) -> String? {
        guard let content else { return nil }
        func with(_ key: String, _ text: String) -> String {
            Localization.text(key, texts: ["text": text])
        }
        switch content {
        case .poll(let poll): return with("chat.content.preview.poll", poll.body.question)
        case .checklist(let list): return with("chat.content.preview.checklist", list.body.title)
        case .location: return Localization.text("attach.menu.location")
        case .contactCard(let card): return with("chat.content.preview.contact", card.name)
        case .article(let article): return with("chat.content.preview.article", article.title)
        }
    }
}
