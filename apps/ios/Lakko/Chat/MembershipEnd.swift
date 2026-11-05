// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// ADR-0087 section 9, `docs/ux/group-and-channel-privacy.md` section 9 — when
/// no device of a person is left in a group, the group goes from every device
/// of that person: row, history and MLS state.
///
/// This file holds what can be asserted without a running repository: the
/// trigger, the verdict on a heard register, the verdict on a held Welcome,
/// and the one durable record the pipeline resumes from
/// (``MembershipEndStore``). The pipeline itself is in
/// `ChatRepository+MembershipEnd.swift`.
public enum MembershipEndRules {

    /// The trigger: a state, not an event. `isActive` alone is wrong: a device
    /// whose own leaf was retired while its account stays in the group
    /// (ADR-0050) is re-admitted and keeps its history. So this also asks the
    /// frozen roster whether any leaf of this account is left.
    ///
    /// An unknown answer is `false` either way: a torn handle or an unreadable
    /// or empty roster is not evidence of an end, and acting on one deletes a
    /// person's history. A group always holds the leaf of whoever committed,
    /// so an empty roster is a read that failed.
    public static func personOut(
        isActive: Bool?, roster: [GroupMember]?, ownKeys: Set<Data>
    ) -> Bool {
        guard isActive == false, let roster, !roster.isEmpty, !ownKeys.isEmpty else { return false }
        return !roster.contains {
            ownKeys.contains(PersonKey.of($0)) || ownKeys.contains($0.signaturePublicKey)
        }
    }

    /// What a `conversation_ended` register asks of this device.
    public enum HeardVerdict: Equatable {
        /// Nothing to act on: the group is not held, the end is already owed,
        /// or the handle cannot be read.
        case nothing
        /// Stop, wipe, tell the person: the group ended at `endedEpoch`.
        case wipe(endedEpoch: UInt64)
        /// This device came back after that end. Stop nothing, wipe nothing,
        /// and tell the other devices with a `joined` at this epoch.
        case answerJoined(epoch: UInt64)
    }

    /// Judge a heard register against what this device holds.
    ///
    /// This comes BEFORE anything is stopped: `deleteAfterEndedElsewhere`
    /// would refuse a rejoined group, but only at the last step, after the
    /// poll and the push subscription had already gone.
    ///
    /// A group that is not active and stands above the end came back and was
    /// then retired: its own commit decides, not an older register.
    public static func heard(
        registerEpoch: UInt64, held: Bool, alreadyOwed: Bool, isActive: Bool?, epoch: UInt64?
    ) -> HeardVerdict {
        guard held, !alreadyOwed, let isActive, let epoch else { return .nothing }
        if epoch > registerEpoch {
            return isActive ? .answerJoined(epoch: epoch) : .nothing
        }
        return .wipe(endedEpoch: registerEpoch)
    }

    /// What to do with a held Welcome now.
    public enum HeldWelcomeStep: Equatable {
        /// The group is gone or ended here: the Welcome can land.
        case join
        /// The end is still being carried out; come back when it is done.
        case waitForWipe
        /// Read the group's journal: the removal may be in it.
        case readJournal
        /// The journal is drained and the group is still active: this is a
        /// duplicate or a forgery. The core does not report a Welcome's
        /// epoch, so no client can tell sooner.
        case drop
    }

    public static func heldWelcomeStep(
        held: Bool, owed: Bool, isActive: Bool?, drained: Bool
    ) -> HeldWelcomeStep {
        guard held else { return .join }
        if owed { return .waitForWipe }
        if isActive == false { return .join }
        return (drained && isActive == true) ? .drop : .readJournal
    }

    /// The group a refused Welcome named, or `nil` for any other failure. The
    /// message of `FfiError.MembershipGroupStillActive` is exactly the
    /// lower-case hex id.
    public static func stillActiveGroupIdHex(_ error: Error) -> String? {
        if case ChatError.wrapped(_, let underlying) = error {
            return stillActiveGroupIdHex(underlying)
        }
        guard case FfiError.MembershipGroupStillActive(let message) = error else { return nil }
        return message
    }
}

/// One end in progress. Every step is idempotent, so a crash at any point
/// resumes here.
public struct OwedMembershipEnd: Codable, Equatable, Sendable {
    public let groupIdHex: String
    /// The MLS epoch of the end: the frozen one, or the register's.
    public let endedEpoch: UInt64
    /// This device heard of it from the register and may not have applied the
    /// commit, so the group goes through `deleteAfterEndedElsewhere`.
    public let heard: Bool
    /// Which kind the register's reference names.
    public let oneToOne: Bool
    /// A channel: `groupIdHex` holds the channel id, and only telling the
    /// other devices is owed (UX 9.5).
    public let channel: Bool
    /// Step 2 is done, or there was nothing to tell.
    public var published: Bool
    /// Step 3 is done.
    public var stopped: Bool

    public init(
        groupIdHex: String, endedEpoch: UInt64, heard: Bool, oneToOne: Bool,
        channel: Bool = false, published: Bool = false, stopped: Bool = false
    ) {
        self.groupIdHex = groupIdHex
        self.endedEpoch = endedEpoch
        self.heard = heard
        self.oneToOne = oneToOne
        self.channel = channel
        self.published = published
        self.stopped = stopped
    }

    /// The kind of the `conversation_ended` reference.
    public var referenceKind: FolderConversationKind {
        if channel { return .channel }
        return oneToOne ? .oneToOne : .group
    }
}

/// UX 9.1 — "You are no longer in {name}.", once per device. The name is held
/// only for this: when the notice closes, it is gone.
public struct MembershipEndNotice: Codable, Equatable, Identifiable, Sendable {
    public let groupIdHex: String
    public let name: String
    public var id: String { groupIdHex }

    public init(groupIdHex: String, name: String) {
        self.groupIdHex = groupIdHex
        self.name = name
    }
}

/// A Welcome for a group this device still holds as active. The mailbox cursor
/// moves past the row, so this is the only copy.
public struct HeldWelcome: Codable, Equatable, Sendable {
    public let groupIdHex: String
    public let welcome: Data

    public init(groupIdHex: String, welcome: Data) {
        self.groupIdHex = groupIdHex
        self.welcome = welcome
    }
}

/// The owed ends, the notices not yet closed and the held Welcomes, per
/// account, as one blob sealed under the message log's wrap key: a notice
/// names a group. Written before anything it stands for is done. Wiped with
/// the account.
public final class MembershipEndStore {

    /// How a held Welcome list is bounded: the newest are kept. Chosen, not
    /// measured: a Welcome waits only until its group's journal is read.
    public static let maxHeldWelcomes = 16

    private struct Payload: Codable, Equatable {
        var owed: [String: OwedMembershipEnd] = [:]
        var notices: [MembershipEndNotice] = []
        var held: [HeldWelcome] = []
    }

    public struct Sealer {
        public var seal: (Data) throws -> Data
        public var open: (Data) throws -> Data

        public init(seal: @escaping (Data) throws -> Data, open: @escaping (Data) throws -> Data) {
            self.seal = seal
            self.open = open
        }

        static let wrapKey = Sealer(
            seal: { try MessageLogStore.sealUnderWrapKey($0) },
            open: { try MessageLogStore.openUnderWrapKey($0) })
    }

    private static let prefix = "lakko.membership-end.v1."

    private let defaults: UserDefaults
    private let sealer: Sealer

    public init(defaults: UserDefaults = .standard, sealer: Sealer? = nil) {
        self.defaults = defaults
        self.sealer = sealer ?? .wrapKey
    }

    private func read(_ accountIdB64: String) -> Payload {
        guard let sealed = defaults.data(forKey: Self.prefix + accountIdB64),
              let plain = try? sealer.open(sealed),
              let payload = try? JSONDecoder().decode(Payload.self, from: plain)
        else { return Payload() }
        return payload
    }

    /// `false` when the write was refused (the account's stores are sealed).
    @discardableResult
    private func update(_ accountIdB64: String, _ change: (inout Payload) -> Void) -> Bool {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        defer { gate.leave() }
        var payload = read(accountIdB64)
        change(&payload)
        guard let plain = try? JSONEncoder().encode(payload),
              let sealed = try? sealer.seal(plain) else { return false }
        defaults.set(sealed, forKey: Self.prefix + accountIdB64)
        return true
    }

    // MARK: Owed ends

    public func owed(accountIdB64: String) -> [OwedMembershipEnd] {
        read(accountIdB64).owed.values.sorted { $0.groupIdHex < $1.groupIdHex }
    }

    public func owed(accountIdB64: String, groupIdHex: String) -> OwedMembershipEnd? {
        read(accountIdB64).owed[groupIdHex]
    }

    /// Step 1 — the end and its notice, in one write.
    @discardableResult
    public func record(
        _ end: OwedMembershipEnd, notice: MembershipEndNotice?, accountIdB64: String
    ) -> Bool {
        update(accountIdB64) { payload in
            payload.owed[end.groupIdHex] = end
            if let notice {
                payload.notices.removeAll { $0.groupIdHex == notice.groupIdHex }
                payload.notices.append(notice)
            }
        }
    }

    public func put(_ end: OwedMembershipEnd, accountIdB64: String) {
        update(accountIdB64) { $0.owed[end.groupIdHex] = end }
    }

    public func removeOwed(accountIdB64: String, groupIdHex: String) {
        update(accountIdB64) { $0.owed[groupIdHex] = nil }
    }

    // MARK: Notices

    public func notices(accountIdB64: String) -> [MembershipEndNotice] {
        read(accountIdB64).notices
    }

    public func removeNotice(accountIdB64: String, groupIdHex: String) {
        update(accountIdB64) { $0.notices.removeAll { $0.groupIdHex == groupIdHex } }
    }

    // MARK: Held Welcomes

    public func heldWelcomes(accountIdB64: String) -> [HeldWelcome] {
        read(accountIdB64).held
    }

    @discardableResult
    public func hold(_ welcome: HeldWelcome, accountIdB64: String) -> Bool {
        update(accountIdB64) { payload in
            guard !payload.held.contains(welcome) else { return }
            payload.held.append(welcome)
            if payload.held.count > Self.maxHeldWelcomes {
                payload.held.removeFirst(payload.held.count - Self.maxHeldWelcomes)
            }
        }
    }

    public func removeHeld(_ welcome: HeldWelcome, accountIdB64: String) {
        update(accountIdB64) { $0.held.removeAll { $0 == welcome } }
    }

    // MARK: Account

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}

/// What the repository needs of the account's synced state. A protocol so a
/// test can stand in for a DeviceGroup.
@MainActor
public protocol ConversationEndSync: AnyObject {
    /// The registers saying a conversation ended, for the fold's own reading.
    var endedConversations: [ConversationEndRegister] { get }
    /// Step 2. `false` when it could not be done yet: the synced state is not
    /// ready. A `nil` compose counts as done: it never stops the wipe.
    func publishConversationEnded(_ conversation: FolderConversationRef, epoch: UInt64) async -> Bool
    /// A join after an end: tell the other devices. No-op when no end is held.
    func publishConversationJoined(_ conversation: FolderConversationRef, epoch: UInt64) async
    /// Clear the conversation's registers through their own tombstones.
    /// `false` when it could not be done yet.
    func clearConversation(_ conversations: [FolderConversationRef]) async -> Bool
}

/// The banner's model. Its own object, so the list is not re-evaluated by it.
@MainActor
public final class MembershipEndNoticeCenter: ObservableObject {
    @Published public private(set) var notices: [MembershipEndNotice] = []

    public init() {}

    func replace(_ notices: [MembershipEndNotice]) {
        if notices != self.notices { self.notices = notices }
    }

    /// The banners at the top of Chats, top to bottom: one per removal not
    /// yet closed, the newest on top (UX 9.1, decided 2026-11-04). A newer
    /// removal never hides an older one; closing one leaves the others.
    public static func stacked(_ notices: [MembershipEndNotice]) -> [MembershipEndNotice] {
        notices.reversed()
    }
}
