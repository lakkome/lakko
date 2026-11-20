// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0092 — sending a poll, checklist, location, contact card or article
// into a group conversation, and every op on one (votes, closes, ticks,
// added items, edits). Anonymous ballots go through ``BallotQueue``.

/// Why a content action did not happen.
public enum ContentSendError: Error, Equatable {
    case notReady
    case compose
    case notAllowed
    /// An anonymous poll whose group changed since it was made: its signed
    /// first-ring hash no longer matches the roster, so receivers would read
    /// it as unverifiable. It stays failed (`poll.resend_refused`).
    case pollRingChanged
}

extension ChatRepository {

    // MARK: Content messages

    /// Send a content message. Polls and checklists are signed here with the
    /// conversation account key; an anonymous poll gets a fresh secret and the
    /// hash of ring version 0 (the roster at the epoch it is sealed at, read
    /// under the same lock the seal runs in).
    public func sendContent(
        groupIdHex: String,
        content draft: ContentDraft,
        attachments: [AttachmentRef] = [],
        forward: ForwardRef? = nil
    ) async throws {
        guard let record = groups[groupIdHex], let author = conversationAccountIdentity else {
            throw ContentSendError.notReady
        }
        // ADR-0073 R.7 case 1 — a location, card or article (a forward above
        // all) into a one-to-one whose successor does not exist yet is HELD
        // with its content and goes out with it.
        if sendRoute(groupIdHex: groupIdHex) == .hold {
            guard let content = draft.unsigned(), HeldSend.holdsContent(content),
                  holdMessage(
                    groupIdHex: groupIdHex, text: "", attachments: attachments,
                    reply: nil, forward: forward, content: content)
            else { throw ChatError.argument("the message could not be kept for sending") }
            return
        }
        let groupId = record.groupId
        let localId = record.nextLocalId()
        let ownKey = AuthorIdentity.own(group: record.group)
        // The pending row carries what can be shown before the seal: the
        // signed form is composed inside the lock (the ring must be the
        // roster of the epoch the seal uses).
        // ADR-0092 section 11a (forwarding, amended 2026-11-18) — a forwarded
        // location, card or article carries the forward line.
        let parts = ContentModel.forwardParts(forward)
        let preview = try draft.compose(groupId: groupId, authorSeed: author.ed25519Seed(), ringKeys: nil)
        guard let previewStored = ContentModel.store(
            content: preview.content, attachments: attachments, parts: parts) else {
            throw ContentSendError.compose
        }
        record.appendPending(
            localId: localId, text: "", attachments: attachments,
            createdAt: Self.isoTimestamp(ms: Self.nowMs()), authorKeyB64: ownKey, forward: forward)
        record.mutateLocalRow(localId: localId) { $0.content = previewStored }
        state = state.with(threads: buildThreadList())
        let result: SendMessageResult
        var sentStored = previewStored
        do {
            result = try await withCommitLock(groupIdHex: groupIdHex) { [self] in
                guard let live = groups[groupIdHex] else { throw ContentSendError.notReady }
                let ringKeys = draft.isAnonymousPoll ? ContentRing.accountKeys(of: live.group) : nil
                let composed = try draft.compose(groupId: groupId, authorSeed: author.ed25519Seed(), ringKeys: ringKeys)
                guard let bytes = try? composeContentEnvelope(
                    content: composed.content, attachments: attachments, parts: parts) else {
                    throw ContentSendError.compose
                }
                var stored = PersistedContent(envelopeB64: bytes.base64EncodedString())
                if let ringKeys, case .poll(let poll) = composed.content,
                   case .ring(let encoded) = ContentRing.ring0(poll: poll, keys: ringKeys) {
                    stored.ops.ringB64 = encoded.base64EncodedString()
                }
                sentStored = stored
                return try await sealAndPublishLocked(groupIdHex: groupIdHex, plaintextBytes: bytes, wake: .message)
            }
        } catch {
            // The failed row keeps what was composed under the lock (an
            // anonymous poll's real first-ring hash and ring), so Retry
            // re-sends exactly that, or refuses it when the group changed.
            let failed = groups[groupIdHex] ?? record
            if sentStored != previewStored {
                failed.mutateLocalRow(localId: localId) { $0.content = sentStored }
            }
            failed.markFailed(localId: localId)
            state = state.with(threads: buildThreadList())
            throw error
        }
        let live = groups[groupIdHex] ?? record
        live.mutateLocalRow(localId: localId) { $0.content = sentStored }
        live.markSent(localId: localId, messageId: result.messageId, createdAt: result.createdAt)
        persistMessageLogBestEffort()
        state = state.with(threads: buildThreadList())
    }

    // MARK: Ops

    /// Vote in (or retract from, with empty `choices`) the poll `pollMessageId`.
    /// A non-anonymous vote is an MLS op; an anonymous one is queued as a
    /// ballot (ADR-0092 section 3.6) and shows as cast at once.
    public func votePoll(groupIdHex: String, pollMessageId: Int64, choices: [UInt8]) async throws {
        guard let record = groups[groupIdHex],
              let message = record.messages.first(where: { $0.messageId == pollMessageId }),
              let stored = message.content, case .poll(let poll)? = ContentModel.content(stored) else {
            throw ContentSendError.notReady
        }
        let sorted = Array(Set(choices)).sorted()
        if poll.body.anonymous != nil {
            BallotQueue.shared.enqueue(groupIdHex: groupIdHex, pollMessageId: pollMessageId, choices: sorted)
            PollChoiceCenter.shared.noteLocal(groupIdHex: groupIdHex, pollMessageId: pollMessageId, choices: sorted)
            objectWillChangeContent()
            return
        }
        let bytes = try composePollVoteEnvelope(target: pollMessageId, choices: Data(sorted))
        let ownKey = conversationAccountIdentity.map { base64Encode($0.signaturePublicKey()) }
        try await publishMessageOpLocked(groupIdHex: groupIdHex, envelopeBytes: bytes) { record, opId in
            guard let ownKey else { return }
            record.mutateMessage(messageId: pollMessageId) {
                $0.content?.ops.votes.append(.init(
                    messageId: opId ?? Int64.max, createdAtMs: Self.nowMs(), voterB64: ownKey, choices: sorted))
            }
        }
        persistMessageLogBestEffort()
    }

    /// Close a poll or a checklist (final). `tally` is a channel poll's frozen
    /// tally (`encodeChannelPollTally`), `nil` in a group.
    public func closeContent(groupIdHex: String, messageId: Int64) async throws {
        let bytes = try ContentFFI.composeClose(target: messageId, tally: nil)
        let own = conversationAccountIdentity.map { base64Encode($0.signaturePublicKey()) }
        try await publishMessageOpLocked(groupIdHex: groupIdHex, envelopeBytes: bytes) { record, opId in
            guard let own else { return }
            let admin = Data(base64Encoded: own).map(record.isOwnerOrAdmin) ?? false
            record.mutateMessage(messageId: messageId) {
                $0.content?.ops.closes.append(.init(messageId: opId ?? Int64.max, closerB64: own, closerIsAdmin: admin))
            }
        }
        persistMessageLogBestEffort()
    }

    /// Tick or untick an item.
    public func tickChecklist(groupIdHex: String, messageId: Int64, item: ChecklistItemRef, done: Bool) async throws {
        let bytes = try composeChecklistTickEnvelope(target: messageId, item: item, done: done)
        let own = conversationAccountIdentity.map { base64Encode($0.signaturePublicKey()) }
        try await publishMessageOpLocked(groupIdHex: groupIdHex, envelopeBytes: bytes) { record, opId in
            guard let own else { return }
            record.mutateMessage(messageId: messageId) {
                $0.content?.ops.ticks.append(.init(
                    messageId: opId ?? Int64.max, tickerB64: own, privileged: false,
                    item: ContentModel.persisted(item), done: done))
            }
        }
        persistMessageLogBestEffort()
    }

    /// Add an item to a checklist.
    public func addChecklistItem(groupIdHex: String, messageId: Int64, text: String) async throws {
        let bytes = try composeChecklistAddEnvelope(target: messageId, text: text)
        let own = conversationAccountIdentity.map { base64Encode($0.signaturePublicKey()) }
        try await publishMessageOpLocked(groupIdHex: groupIdHex, envelopeBytes: bytes) { record, opId in
            guard let own, let opId else { return }
            record.mutateMessage(messageId: messageId) {
                $0.content?.ops.adds.append(.init(messageId: opId, adderB64: own, privileged: false, text: text))
            }
        }
        persistMessageLogBestEffort()
    }

    /// The author's edit of a checklist (title, items, order, the two flags).
    public func editChecklist(groupIdHex: String, messageId: Int64, body: ChecklistBody) async throws {
        guard let record = groups[groupIdHex], let author = conversationAccountIdentity else {
            throw ContentSendError.notReady
        }
        let signed = try signChecklist(body: body, groupId: record.groupId, authorSeed: author.ed25519Seed())
        let bytes = try composeChecklistEditEnvelope(target: messageId, checklist: signed)
        let own = base64Encode(author.signaturePublicKey())
        try await publishMessageOpLocked(groupIdHex: groupIdHex, envelopeBytes: bytes) { record, opId in
            record.mutateMessage(messageId: messageId) {
                $0.content?.ops.edits.append(.init(
                    messageId: opId ?? Int64.max, editorB64: own, envelopeB64: bytes.base64EncodedString()))
            }
        }
        persistMessageLogBestEffort()
    }

    /// The author's edit of an article: the whole article and its images.
    public func editArticle(
        groupIdHex: String, messageId: Int64, article: Article, attachments: [AttachmentRef]
    ) async throws {
        let bytes = try composeArticleEditEnvelope(target: messageId, article: article, attachments: attachments)
        try await publishMessageOpLocked(groupIdHex: groupIdHex, envelopeBytes: bytes) { record, opId in
            guard let replaced = ContentModel.store(content: .article(article: article), attachments: attachments)
            else { return }
            record.mutateMessage(messageId: messageId) { m in
                m.content?.envelopeB64 = replaced.envelopeB64
                m.attachments = attachments
                m.edited = true
                m.noteOp(opId)
            }
        }
        persistMessageLogBestEffort()
    }

    /// Republish the thread list after a local-only change.
    func objectWillChangeContent() {
        state = state.with(threads: buildThreadList())
    }

    /// Post an anonymous ballot row to `groupIdHex`'s journal (a non-MLS row,
    /// proven with the current epoch's access key, no wake).
    func postBallotRow(groupIdHex: String, row: Data) async throws {
        guard let identity = canonicalIdentity, let record = groups[groupIdHex] else {
            throw ContentSendError.notReady
        }
        // Walk-3 rule W3-R5 — a vote is the person's own act: held while the
        // conversation is behind (the ballot shows as failed, Try again), and
        // re-sent once back in step.
        try requireSendAllowedWhileBehind(groupIdHex: groupIdHex, isPersonContent: false, personAction: true)
        try await chatClient.publishRawRow(identity: identity, group: record.group, row: row)
    }

    /// What a ballot needs from the poll: keys, ring and the voter key. `nil`
    /// when the poll is not held, not anonymous or unverifiable.
    func ballotInputs(groupIdHex: String, pollMessageId: Int64)
        -> (keys: PollBallotKeys, ring: PollRing, voter: PollVoterKey, verified: [VerifiedPollBallot], open: Bool)? {
        guard let record = groups[groupIdHex], let seed = conversationAccountIdentity?.ed25519Seed(),
              let message = record.messages.first(where: { $0.messageId == pollMessageId }),
              let stored = message.content, !stored.ops.unverifiable,
              case .poll(let poll)? = ContentModel.content(stored), let anon = poll.body.anonymous,
              let ringBytes = stored.ops.ringB64.flatMap({ Data(base64Encoded: $0) }),
              let ring = try? PollRing.decode(bytes: ringBytes),
              let keys = try? PollBallotKeys(groupId: record.groupId, pollId: poll.body.id, secret: anon.secret),
              let voter = try? PollVoterKey.fromEd25519Seed(seed: seed) else { return nil }
        let verified = stored.ops.ballots.compactMap { b -> VerifiedPollBallot? in
            guard let tag = b.tagB64.flatMap({ Data(base64Encoded: $0) }), let choices = b.choices,
                  let version = b.version else { return nil }
            return VerifiedPollBallot(tag: tag, choices: Data(choices), version: version)
        }
        let open = PollView.make(message: message, ownAccountKey: nil, registerChoice: nil, nowMs: Self.nowMs())
            .map { !$0.closed } ?? false
        return (keys, ring, voter, verified, open)
    }

    /// The distinct account keys of a group's roster.
    func memberAccountKeys(groupIdHex: String) -> [Data] {
        groups[groupIdHex].map { ContentRing.accountKeys(of: $0.group) } ?? []
    }

    /// The group's conversation reference for the synced registers.
    func folderRef(groupIdHex: String) -> FolderConversationRef? {
        guard let record = groups[groupIdHex] else { return nil }
        return FolderConversationRef(kind: isOneToOne(groupIdHex: groupIdHex) ? .oneToOne : .group, id: record.groupId)
    }
}

/// One wrapper per FFI call whose signature another agent is changing, so the
/// change is one line here.
enum ContentFFI {
    /// `composePollCloseEnvelope`, with a channel poll's frozen tally.
    static func composeClose(target: Int64, tally: Data?) throws -> Data {
        try composePollCloseEnvelope(target: target, tally: tally)
    }

    /// A close's frozen channel tally (`ParsedBody.pollTally`).
    static func pollTally(_ parsed: ParsedBody) -> ChannelPollTally? {
        parsed.pollTally
    }
}

// MARK: - What an editor hands over

/// ADR-0092 — a content message as an editor produced it, before signing.
public enum ContentDraft {
    case poll(question: String, options: [String], multiple: Bool, quiz: PollQuiz?, closeAtMs: Int64?, anonymous: Bool)
    case checklist(ChecklistBody)
    case location(SharedLocation)
    case contactCard(SharedContactCard)
    case article(Article)

    /// A channel post's content: no author signature (the post signature is
    /// the author's) and never the anonymous fields (every channel poll is
    /// anonymous through the credential, ADR-0092 section 4.1).
    func unsigned() -> MessageContent? {
        switch self {
        case .poll(let question, let options, let multiple, let quiz, let closeAtMs, _):
            let body = PollBody(
                id: ContentSecrets.random(16), question: question, options: options, multiple: multiple,
                quiz: quiz, closeAtMs: closeAtMs, anonymous: nil)
            return .poll(poll: SignedPoll(body: body, authorSig: nil))
        case .checklist(let body):
            return .checklist(checklist: SignedChecklist(body: body, authorSig: nil))
        case .location(let location): return .location(location: location)
        case .contactCard(let card): return .contactCard(card: card)
        case .article(let article): return .article(article: article)
        }
    }

    var isAnonymousPoll: Bool {
        if case .poll(_, _, _, _, _, true) = self { return true }
        return false
    }

    /// Sign (poll, checklist) and wrap. `ringKeys` is the roster for an
    /// anonymous poll; `nil` composes a placeholder hash (the pending row).
    func compose(groupId: Data, authorSeed: Data, ringKeys: [Data]?) throws -> (content: MessageContent, Void) {
        switch self {
        case .poll(let question, let options, let multiple, let quiz, let closeAtMs, let anonymous):
            var anon: PollAnonymous?
            if anonymous {
                let hash = try ringKeys.map { try pollRingHash(keys: $0) } ?? Data(repeating: 0, count: 32)
                anon = PollAnonymous(secret: ContentSecrets.random(32), ring0Hash: hash)
            }
            let body = PollBody(
                id: ContentSecrets.random(16), question: question, options: options, multiple: multiple,
                quiz: quiz, closeAtMs: closeAtMs, anonymous: anon)
            return (.poll(poll: try signPoll(body: body, groupId: groupId, authorSeed: authorSeed)), ())
        case .checklist(let body):
            return (.checklist(checklist: try signChecklist(body: body, groupId: groupId, authorSeed: authorSeed)), ())
        case .location(let location):
            return (.location(location: location), ())
        case .contactCard(let card):
            return (.contactCard(card: card), ())
        case .article(let article):
            return (.article(article: article), ())
        }
    }
}

extension HeldSend {
    /// ADR-0092 — the content a held one-to-one keeps for its successor: a
    /// location, contact card or article, which carry no signature over the
    /// group. A poll or checklist is signed over the group it was composed
    /// in, so it is never held for another.
    public static func holdsContent(_ content: MessageContent) -> Bool {
        switch content {
        case .location, .contactCard, .article: return true
        case .poll, .checklist: return false
        }
    }
}

enum ContentSecrets {
    static func random(_ count: Int) -> Data {
        var bytes = [UInt8](repeating: 0, count: count)
        _ = SecRandomCopyBytes(kSecRandomDefault, count, &bytes)
        return Data(bytes)
    }
}
