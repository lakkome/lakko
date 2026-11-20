// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// What a report is about (UX `reporting-and-moderation.md` section 2.1).
public enum ReportTarget: Equatable {
    /// A person, by their 32-byte account key.
    case person(accountKey: Data)
    /// A group, by its `@name` (a group is reported only by its `@name`).
    case group(handle: String)
    /// A channel, by its 16-byte id.
    case channel(channelId: Data)

    /// The wire target kind (`sealModerationReport`).
    var kindByte: UInt8 {
        switch self {
        case .person: return 1
        case .group: return 2
        case .channel: return 3
        }
    }
}

/// Where the report was made from (`conversation_kind` on the wire).
public enum ReportConversationKind: UInt8 {
    case oneToOne = 1, group = 2, channel = 3, contactRequest = 4, discover = 5, card = 6
}

/// What the "Also ..." checkbox does when the report goes.
public enum ReportFollowUp: Equatable {
    /// `Also block {name}`, on by default.
    case block
    /// `Leave the group`, off by default.
    case leave
    /// `Unsubscribe`, off by default.
    case unsubscribe

    var onByDefault: Bool { self == .block }
}

/// One message a report may carry, from a conversation this device holds.
public struct ReportCandidate: Equatable, Identifiable {
    public let messageId: Int64
    public let authorName: String?
    public let text: String
    public let createdAt: String
    public var id: Int64 { messageId }
}

/// Everything the sheet is opened with.
public struct ReportRequest: Identifiable {
    public let id = UUID()
    public let target: ReportTarget
    /// The name of the target as the reporter sees it (Your reports).
    public let targetName: String
    public let conversationKind: ReportConversationKind
    /// The conversation messages are picked from, if any.
    public let groupIdHex: String?
    /// Messages already in the report (the message a menu was opened on).
    public let initialMessageIds: [Int64]
    /// Evidence that is not a held message (a contact request, a post, a
    /// listing). Always included.
    public let fixedEvidence: [FfiEvidenceItem]
    /// The messages `Add messages` offers; empty hides the link.
    public let candidates: [ReportCandidate]
    public let followUp: ReportFollowUp?
    /// The name in `Also block {name}` and `{name} isn't told`.
    public let personName: String?
    /// The evidence of a picked candidate when it is not a held chat message
    /// (a channel post); `nil` reads the message evidence kept at receipt.
    public let candidateEvidence: ((Int64) -> FfiEvidenceItem?)?

    public init(
        target: ReportTarget, targetName: String, conversationKind: ReportConversationKind,
        groupIdHex: String? = nil, initialMessageIds: [Int64] = [],
        fixedEvidence: [FfiEvidenceItem] = [], candidates: [ReportCandidate] = [],
        followUp: ReportFollowUp? = nil, personName: String? = nil,
        candidateEvidence: ((Int64) -> FfiEvidenceItem?)? = nil
    ) {
        self.candidateEvidence = candidateEvidence
        self.target = target
        self.targetName = targetName
        self.conversationKind = conversationKind
        self.groupIdHex = groupIdHex
        self.initialMessageIds = initialMessageIds
        self.fixedEvidence = fixedEvidence
        self.candidates = candidates
        self.followUp = followUp
        self.personName = personName
    }
}

/// The report sheet's state (UX 2.2 and 2.3). Everything with an effect is a
/// port, so the states can be driven by a test.
@MainActor
public final class ReportSheetModel: ObservableObject {

    /// UX 2.2 — up to 20 messages (chosen, not measured).
    public static let maxMessages = 20
    /// UX 2.2 — up to 500 characters (chosen).
    public static let maxComment = 500

    public enum Phase: Equatable {
        case editing
        case sending
        /// "Couldn't send the report. Try again." The sheet stays filled in.
        case failed
        /// "You've sent many reports today..." The sheet stays filled in.
        case limited
        /// "Reporting isn't available on this server."
        case unavailable
        /// The report went; the sheet closes.
        case sent
    }

    public struct Ports {
        /// The verified moderator list and the pinned root key, or `nil`
        /// with `unavailable` (no pinned key, service off, older list) or
        /// `failed` (no network).
        public var moderatorList: () async -> ModerationClient.ListResult
        public var rootKey: Data?
        /// The 64-byte OPRF lookup key of a group's `@name`.
        public var groupLookupKey: (String) async -> Data?
        /// The evidence item of a picked message (or post), `nil` when it is
        /// no longer on this device.
        public var evidence: (Int64) -> FfiEvidenceItem?
        public var send: (Data) async -> ModerationClient.SendResult
        /// Record a sent report in Your reports.
        public var record: (_ receiptId: String, _ reason: UInt8) -> Void
        /// Apply the checked follow-up (block, leave, unsubscribe).
        public var applyFollowUp: (ReportFollowUp) -> Void
        public var seal: (
            _ kind: UInt8, _ target: Data, _ reason: UInt8, _ conversationKind: UInt8,
            _ comment: String, _ evidence: [FfiEvidenceItem], _ list: Data, _ rootKey: Data
        ) throws -> Data = { kind, target, reason, conversationKind, comment, evidence, list, rootKey in
            try sealModerationReport(
                targetKind: kind, target: target, reason: reason, conversationKind: conversationKind,
                comment: comment, evidence: evidence, moderatorList: list, rootKey: rootKey)
        }
        public var accountTarget: (Data) throws -> Data = { try moderationAccountTarget(accountKey: $0) }
    }

    public let request: ReportRequest
    private let ports: Ports

    @Published public var reason: UInt8?
    @Published public var comment: String = "" {
        didSet {
            if comment.count > Self.maxComment { comment = String(comment.prefix(Self.maxComment)) }
        }
    }
    @Published public private(set) var picked: [Int64]
    @Published public var followUpChecked: Bool
    @Published public private(set) var phase: Phase = .editing

    public init(request: ReportRequest, ports: Ports) {
        self.request = request
        self.ports = ports
        self.picked = Array(request.initialMessageIds.prefix(Self.maxMessages))
        self.followUpChecked = request.followUp?.onByDefault ?? false
    }

    /// The reasons in the order the core fixes, with their catalog keys.
    public static var reasons: [(code: UInt8, key: String)] {
        reportReasonCodes().compactMap { code in
            reportReasonName(code: code).map { (code, "report.reason." + $0.replacingOccurrences(of: "-", with: "_")) }
        }
    }

    /// How many picked messages still have their evidence on this device.
    public var includedCount: Int { picked.filter { ports.evidence($0) != nil }.count + request.fixedEvidence.count }
    /// Everything that was asked to be included.
    public var requestedCount: Int { picked.count + request.fixedEvidence.count }

    /// The "Included" line (UX 2.2, 2.3).
    public enum IncludedLine: Equatable {
        case none
        case count(Int)
        case partial(included: Int, total: Int)
    }

    public var includedLine: IncludedLine {
        let total = requestedCount
        let included = includedCount
        if total == 0 { return .none }
        if included < total { return .partial(included: included, total: total) }
        return .count(included)
    }

    /// `Send report` needs a reason, takes no second tap while sending, and
    /// stays off once the server said reporting is not available here (UX
    /// 2.3: said when the sheet opens, with `Send report` disabled).
    public var canSend: Bool { reason != nil && phase != .sending && phase != .sent && phase != .unavailable }

    /// UX 2.3, "Reporting not available on this server": asked when the
    /// sheet opens. When the server has no reporting (no pinned key, the
    /// service off, a list refused) the sheet says so and `Send report` is
    /// disabled, so the follow-up is never offered against a report that
    /// cannot go. No answer (no network) leaves the sheet as it is; a send
    /// then says what happened. Seals and sends nothing.
    public func checkAvailability() async {
        guard phase == .editing else { return }
        let unavailable: Bool
        if ports.rootKey == nil {
            unavailable = true
        } else if case .unavailable = await ports.moderatorList() {
            unavailable = true
        } else {
            unavailable = false
        }
        if unavailable, phase == .editing { phase = .unavailable }
    }

    public func isPicked(_ id: Int64) -> Bool { picked.contains(id) }

    /// Toggle a message in select mode. Returns `false` when the limit
    /// stopped an add.
    @discardableResult
    public func toggle(_ id: Int64) -> Bool {
        if let i = picked.firstIndex(of: id) {
            picked.remove(at: i)
            return true
        }
        guard picked.count < Self.maxMessages else { return false }
        picked.append(id)
        return true
    }

    public var atLimit: Bool { picked.count >= Self.maxMessages }

    /// Send (UX 2.3). A second tap while sending does nothing.
    public func send() async {
        guard let reason, phase != .sending, phase != .sent, phase != .unavailable else { return }
        phase = .sending
        guard let rootKey = ports.rootKey else { return finish(.unavailable) }
        let list: Data
        switch await ports.moderatorList() {
        case .list(let l): list = l
        case .unavailable: return finish(.unavailable)
        case .failed: return finish(.failed)
        }
        let target: Data
        switch request.target {
        case .person(let key):
            guard let t = try? ports.accountTarget(key) else { return finish(.failed) }
            target = t
        case .group(let handle):
            guard let t = await ports.groupLookupKey(handle) else { return finish(.failed) }
            target = t
        case .channel(let id):
            target = id
        }
        let evidence = request.fixedEvidence + picked.compactMap { ports.evidence($0) }
        let sealed: Data
        do {
            sealed = try ports.seal(
                request.target.kindByte, target, reason, request.conversationKind.rawValue,
                comment, evidence, list, rootKey)
        } catch {
            NSLog("report: not sealed")
            return finish(.failed)
        }
        switch await ports.send(sealed) {
        case .sent(let receiptId):
            ports.record(receiptId, reason)
            finish(.sent)
        case .limited: finish(.limited)
        case .unavailable: finish(.unavailable)
        case .failed: finish(.failed)
        }
    }

    /// The follow-up applies with a sent report and also when the report
    /// could not go for the limit or an unavailable server (UX 2.3: "The
    /// block still applies if checked"), never on a network failure, where
    /// the person tries again.
    private func finish(_ next: Phase) {
        phase = next
        if let followUp = request.followUp, followUpChecked,
           next == .sent || next == .limited || next == .unavailable {
            ports.applyFollowUp(followUp)
            // Applied once: a later retry does not apply it again.
            followUpChecked = false
        }
    }
}

/// One report this device sent (UX 2.3, Your reports). On this device only:
/// never synced, never in the backup, because syncing it would tie the
/// reports together.
public struct SentReport: Codable, Equatable, Identifiable {
    public let receiptId: String
    public let targetName: String
    public let reason: UInt8
    public let sentAtMs: Int64
    public var id: String { receiptId }
}

@MainActor
public final class ReportHistoryStore: ObservableObject {
    public static let shared = ReportHistoryStore()

    private let file = DeviceLocalSealedFile<[SentReport]>(directory: "lakko-sent-reports", fallback: [])

    init() {}

    /// Newest first.
    public func reports(accountIdB64: String) -> [SentReport] {
        file.read(accountIdB64: accountIdB64).sorted { $0.sentAtMs > $1.sentAtMs }
    }

    public func add(accountIdB64: String, _ report: SentReport) {
        var all = file.read(accountIdB64: accountIdB64)
        all.removeAll { $0.receiptId == report.receiptId }
        all.append(report)
        file.write(accountIdB64: accountIdB64, all)
        objectWillChange.send()
    }

    public func deleteAll(accountIdB64: String) {
        file.delete(accountIdB64: accountIdB64)
        objectWillChange.send()
    }
}

/// UX 2.3 — "Report sent. Moderators will review it.", a short notice at the
/// bottom of whatever screen is up.
@MainActor
public final class ReportNoticeCenter: ObservableObject {
    public static let shared = ReportNoticeCenter()
    public struct Shown: Equatable, Identifiable {
        public let id = UUID()
        public let text: String
    }
    @Published public private(set) var shown: Shown?
    private var hideTask: Task<Void, Never>?

    public func show(_ text: String) {
        shown = Shown(text: text)
        hideTask?.cancel()
        hideTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 4_000_000_000)
            guard !Task.isCancelled else { return }
            self?.shown = nil
        }
    }
}
