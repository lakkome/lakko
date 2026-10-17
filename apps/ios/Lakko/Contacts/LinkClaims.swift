// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0079 (reworked 2026-10-17) — only one of the inviter's devices answers
/// a request sent through one of the account's invite links: CLAIM, THEN
/// BUILD, on the account's own DeviceGroup journal (the pattern ADR-0073 R.6
/// uses for successor groups, ``SuccessorClaim``).
///
///   1. a device that decides `accept` seals a claim naming the request,
///      records the sealed bytes, then publishes them into its DeviceGroup;
///      it builds nothing yet;
///   2. it reads its DeviceGroup journal past the claim and asks the core
///      (`linkClaimOutcome`): the EARLIEST claim for the request from a
///      device in the current roster wins, and the device whose leaf posted
///      it builds;
///   3. only the winner founds the one-to-one, sends the Welcome, records the
///      contact and counts the use;
///   4. a device that lost, or holds no claim and sees one from a device in
///      the roster, keeps the request unshown until the link register says
///      it was answered, checking again against the current roster at every
///      pass: if the claim that beat it came from a device since removed,
///      the next earliest current claim wins (possibly this device's), so a
///      removed device cannot leave a request unanswered.
///
/// A device recognises its OWN claim rows by their bytes: it cannot decrypt
/// its own MLS application messages, so it records each sealed claim before
/// the publish and the DeviceGroup receive loop matches every row against
/// those bytes. A crash between the publish and the recording of its id
/// loses nothing.
///
/// iOS mirror of `apps/web/src/contacts/link-claims.ts`. Nothing here is
/// secret: a request key is a link id and a public account key, a seen claim
/// is a message id and a public leaf key, a sealed claim is ciphertext
/// already handed to the DS.
public enum LinkClaims {

    /// A claim another device of the account posted: its DS message id and
    /// the leaf key (hex) resolved from the MLS sender at receipt.
    public struct Observed: Equatable, Codable {
        public let id: Int64
        public let author: String

        public init(id: Int64, author: String) {
            self.id = id
            self.author = author
        }
    }

    /// One account a founded one-to-one's Welcome goes to.
    public struct FoundedTarget: Equatable, Codable {
        public let accountKeyB64: String
        public let isRecipient: Bool

        public init(accountKeyB64: String, isRecipient: Bool) {
            self.accountKeyB64 = accountKeyB64
            self.isRecipient = isRecipient
        }
    }

    /// The one-to-one this device founded for a request it won, recorded
    /// BEFORE any Welcome is posted, so a retry (a failed publish, a
    /// relaunch) delivers the SAME group's Welcome instead of founding a
    /// second one.
    public struct Founded: Equatable, Codable {
        public let groupIdB64: String
        /// The Welcome (base64); emptied once every target was written.
        public var welcomeB64: String
        /// In delivery order: this account's own mailbox first.
        public let targets: [FoundedTarget]
        public var delivered: Bool

        public init(groupIdB64: String, welcomeB64: String, targets: [FoundedTarget], delivered: Bool) {
            self.groupIdB64 = groupIdB64
            self.welcomeB64 = welcomeB64
            self.targets = targets
            self.delivered = delivered
        }
    }

    /// The order a founded one-to-one's Welcome is written in: this
    /// account's own mailbox (its other devices) FIRST, the requester's after
    /// it. So whenever the requester holds the Welcome, every own device's
    /// mailbox holds it too, and a device that takes the request over finds
    /// the one-to-one before it could found a second. Stable within each half.
    public static func welcomeOrder<T>(_ targets: [T], isRecipient: (T) -> Bool) -> [T] {
        targets.filter { !isRecipient($0) } + targets.filter { isRecipient($0) }
    }

    /// What a device that won a request does before it builds anything.
    public enum BuildStep: Equatable {
        /// A one-to-one was founded for it already: deliver that one's Welcome.
        case resume
        /// The account mailbox could not be read past now: decide next pass.
        case wait
        /// A one-to-one with the requester is held already: answered, build
        /// nothing.
        case answered
        /// Found the one-to-one.
        case found
    }

    /// Before any device builds a one-to-one for a request (the first winner,
    /// or a device that took it over because the winner left the roster), it
    /// reads its account mailbox past now and looks for a one-to-one with the
    /// requester.
    public static func buildStep(hasFounded: Bool, mailboxCaughtUp: Bool, holdsOneToOne: Bool) -> BuildStep {
        if hasFounded { return .resume }
        if !mailboxCaughtUp { return .wait }
        return holdsOneToOne ? .answered : .found
    }

    /// A held conversation as the one-to-one check reads it.
    public struct HeldConversation: Equatable {
        public let oneToOne: Bool
        public let memberAccountKeysHex: [String]

        public init(oneToOne: Bool, memberAccountKeysHex: [String]) {
            self.oneToOne = oneToOne
            self.memberAccountKeysHex = memberAccountKeysHex
        }
    }

    /// Whether one of `conversations` is a one-to-one with the account whose
    /// key is `requesterKeyHex`.
    public static func holdsOneToOneWith(_ conversations: [HeldConversation], requesterKeyHex: String) -> Bool {
        let want = requesterKeyHex.lowercased()
        return conversations.contains { c in
            c.oneToOne && c.memberAccountKeysHex.contains { $0.lowercased() == want }
        }
    }

    /// This device's own claims for one request.
    public struct Own: Equatable, Codable {
        /// Sealed claim messages (hex), recorded BEFORE their publish and not
        /// yet matched to a journal row.
        public var wires: [String] = []
        /// DS message ids of this device's claims on the journal.
        public var ids: [Int64] = []
        /// Decided: this device answers the request, at this claim. Kept
        /// until the build has landed, so a failed build is retried.
        public var won: Int64?
        /// Decided lost: every claim of this device is on the journal and
        /// known, so the next passes re-check against the current roster
        /// without reading the journal again.
        public var settled: Bool = false
        /// The one-to-one founded for the request (won claims only).
        public var founded: Founded?

        public init(
            wires: [String] = [], ids: [Int64] = [], won: Int64? = nil, settled: Bool = false,
            founded: Founded? = nil
        ) {
            self.wires = wires
            self.ids = ids
            self.won = won
            self.settled = settled
            self.founded = founded
        }

        /// Whether this device holds any claim for the request.
        public var holdsClaim: Bool { won != nil || !ids.isEmpty || !wires.isEmpty }
    }

    /// What the device does with a request carrying a link answer, now.
    public enum Step: Equatable {
        /// The synced state is not loaded yet: keep it undecided, unshown.
        case wait
        /// Answered (by another device, or the register says so): remove.
        case drop
        /// The link no longer works: show the card with the note.
        case cardDead
        /// Live link: post this device's claim.
        case postClaim
        /// Claim posted: read the journal past it and decide.
        case decide
        /// Lost: decide again against the current roster (no journal read).
        case recheck
        /// Claim won: found the one-to-one (again, after a failure).
        case build
    }

    /// The next step for one request.
    ///
    /// | own claims        | answered elsewhere | verdict  | step        |
    /// |-------------------|--------------------|----------|-------------|
    /// | won               | any                | any      | `build`     |
    /// | any other         | any                | answered | `drop`      |
    /// | lost (settled)    | any                | dead     | `drop`      |
    /// | lost (settled)    | any                | other    | `recheck`   |
    /// | posted or posting | any                | any      | `decide`    |
    /// | none              | yes                | dead     | `drop`      |
    /// | none              | yes                | other    | `wait`      |
    /// | none              | no                 | not held | `wait`      |
    /// | none              | no                 | dead     | `cardDead`  |
    /// | none              | no                 | accept   | `postClaim` |
    ///
    /// "Answered elsewhere" is the core's `linkClaimAnsweredElsewhere`: a
    /// claim from a device in the current roster. A request goes only when
    /// the register says it was answered, or the link is dead while another
    /// device holds the claim that answers it.
    public static func step(own: Own?, answeredElsewhere: Bool, verdict: LinkAnswerVerdictValue?) -> Step {
        if let own, own.holdsClaim, own.won != nil { return .build }
        if verdict == .answered { return .drop }
        if let own, own.holdsClaim {
            if !own.settled { return .decide }
            return verdict == .dead ? .drop : .recheck
        }
        if answeredElsewhere { return verdict == .dead ? .drop : .wait }
        guard let verdict else { return .wait }
        switch verdict {
        case .answered: return .drop
        case .dead: return .cardDead
        case .accept: return .postClaim
        }
    }

    /// A card already shown for a request (the link no longer worked here)
    /// that a roster device claimed: the request is answered.
    public static func shownCardAnswered(own: Own?, answeredElsewhere: Bool) -> Bool {
        !(own?.holdsClaim ?? false) && answeredElsewhere
    }

    /// The claims the core decides over: this device's own (authored by its
    /// own leaf key) and the others seen.
    public static func claimsToDecide(own: Own?, seen: [Observed], ownKeyHex: String) -> [Observed] {
        (own?.ids ?? []).map { Observed(id: $0, author: ownKeyHex) } + seen
    }

    /// The register's verdict as the rule reads it (the core's
    /// `LinkAnswerVerdict`, kept separate so the rule is testable without
    /// the binding).
    public enum LinkAnswerVerdictValue: Equatable { case accept, answered, dead }

    /// Lower-case hex of `link_id ‖ requester_account_key`.
    public static func requestKeyHex(linkId: Data, requesterKey: Data) -> String {
        hex(linkId + requesterKey)
    }

    /// Lower-case hex of `bytes`.
    public static func hex(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// `hex` decoded, or `nil`.
    public static func bytes(hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            out.append(byte)
            index = next
        }
        return out
    }

    /// How many requests' seen claims are kept; past it the oldest go.
    /// Chosen, not measured (the web value).
    public static let maxSeen = 512

    /// How many claims per request are kept (the lowest ids). Chosen, not
    /// measured: one per device, at most 8 conversation leaves (ADR-0050).
    public static let maxClaimsPerRequest = 16

    /// One account's claim record in `UserDefaults`
    /// (`me.lakko.contacts.link-claims.v1.<account>`). Every write is one
    /// synchronous `set` under a lock, so a decision in progress and the
    /// receive loop never interleave a read-modify-write.
    public final class Record {
        private struct Seen: Codable, Equatable {
            var claims: [Observed]
            var at: Int64
        }

        private struct JoinedWelcome: Codable, Equatable {
            var welcomeB64: String
            var at: Int64
        }

        private struct Body: Codable, Equatable {
            var v: Int = 2
            var seen: [String: Seen] = [:]
            var own: [String: Own] = [:]
            /// The Welcome each recently joined one-to-one was joined from,
            /// by group id: a device that takes a request over forwards it.
            var joined: [String: JoinedWelcome]? = [:]
        }

        /// How many joined one-to-one Welcomes are kept. Chosen, not
        /// measured (the web value).
        public static let maxJoinedWelcomes = 32

        static let prefix = "me.lakko.contacts.link-claims.v1."
        private static let lock = NSLock()

        private let defaults: UserDefaults
        private let accountIdB64: String

        public init(accountIdB64: String, defaults: UserDefaults = .standard) {
            self.accountIdB64 = accountIdB64
            self.defaults = defaults
        }

        static func storageKey(_ accountIdB64: String) -> String { prefix + accountIdB64 }

        private func read() -> Body {
            guard let data = defaults.data(forKey: Self.storageKey(accountIdB64)),
                  let body = try? JSONDecoder().decode(Body.self, from: data),
                  body.v == 2
            else { return Body() }
            return body
        }

        @discardableResult
        private func write(_ body: Body) -> Bool {
            guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
            defer { gate.leave() }
            guard let data = try? JSONEncoder().encode(body) else { return false }
            defaults.set(data, forKey: Self.storageKey(accountIdB64))
            return true
        }

        private func update(_ f: (inout Body) -> Bool) -> Bool {
            Self.lock.lock()
            defer { Self.lock.unlock() }
            var body = read()
            guard f(&body) else { return false }
            return write(body)
        }

        /// The claims other devices of the account posted for the request.
        public func seen(_ keyHex: String) -> [Observed] {
            Self.lock.lock()
            defer { Self.lock.unlock() }
            return read().seen[keyHex]?.claims ?? []
        }

        /// Record a claim another device posted, with the leaf key resolved
        /// at receipt. `true` when the record changed.
        @discardableResult
        public func recordSeen(_ keyHex: String, messageId: Int64, authorHex: String, nowMs: Int64) -> Bool {
            update { body in
                var claims = body.seen[keyHex]?.claims ?? []
                guard !claims.contains(where: { $0.id == messageId }) else { return false }
                claims.append(Observed(id: messageId, author: authorHex))
                claims.sort { $0.id < $1.id }
                claims = Array(claims.prefix(LinkClaims.maxClaimsPerRequest))
                guard claims.contains(where: { $0.id == messageId }) else { return false }
                body.seen[keyHex] = Seen(claims: claims, at: nowMs)
                if body.seen.count > LinkClaims.maxSeen {
                    let oldest = body.seen.sorted { $0.value.at < $1.value.at }
                    for (k, _) in oldest.prefix(body.seen.count - LinkClaims.maxSeen) { body.seen[k] = nil }
                }
                return true
            }
        }

        public func own(_ keyHex: String) -> Own? {
            Self.lock.lock()
            defer { Self.lock.unlock() }
            return read().own[keyHex]
        }

        /// Record a sealed claim BEFORE its publish. `false` when the write
        /// failed: a claim must not be posted unrecorded.
        @discardableResult
        public func addOwnWire(_ keyHex: String, wireHex: String) -> Bool {
            update { body in
                var own = body.own[keyHex] ?? Own()
                own.wires.append(wireHex)
                body.own[keyHex] = own
                return true
            }
        }

        /// A journal row (or a publish answer) carried these sealed bytes at
        /// this id: if they are one of this device's recorded claims, file
        /// the id as its own. `true` when they were.
        @discardableResult
        public func recordOwnClaimRow(wireHex: String, messageId: Int64) -> Bool {
            update { body in
                guard let key = body.own.first(where: { $0.value.wires.contains(wireHex) })?.key,
                      var own = body.own[key]
                else { return false }
                own.wires.removeAll { $0 == wireHex }
                if !own.ids.contains(messageId) {
                    own.ids.append(messageId)
                    own.ids.sort()
                }
                body.own[key] = own
                return true
            }
        }

        /// `true` when some sealed claim still waits for its row.
        public func hasUnmatchedWires() -> Bool {
            Self.lock.lock()
            defer { Self.lock.unlock() }
            return read().own.values.contains { !$0.wires.isEmpty }
        }

        /// After a read of the journal past every claim: sealed claims never
        /// matched to a row did not land; drop them.
        public func dropUnlanded(_ keyHex: String) {
            _ = update { body in
                guard var own = body.own[keyHex], !own.wires.isEmpty else { return false }
                own.wires = []
                body.own[keyHex] = own
                return true
            }
        }

        /// Record the founded one-to-one (before any Welcome is posted), or
        /// its delivery. `false` when the write failed: nothing may be posted
        /// for a founding this device would not find again.
        @discardableResult
        public func setFounded(_ keyHex: String, _ founded: Founded) -> Bool {
            update { body in
                var own = body.own[keyHex] ?? Own()
                own.founded = founded
                body.own[keyHex] = own
                return true
            }
        }

        /// Keep the Welcome a one-to-one was joined from (the newest
        /// ``maxJoinedWelcomes``).
        @discardableResult
        public func recordJoinedWelcome(_ groupIdHex: String, welcomeB64: String, nowMs: Int64) -> Bool {
            update { body in
                var joined = body.joined ?? [:]
                joined[groupIdHex] = JoinedWelcome(welcomeB64: welcomeB64, at: nowMs)
                if joined.count > Self.maxJoinedWelcomes {
                    let oldest = joined.sorted { $0.value.at < $1.value.at }
                    for (k, _) in oldest.prefix(joined.count - Self.maxJoinedWelcomes) { joined[k] = nil }
                }
                body.joined = joined
                return true
            }
        }

        /// The Welcome a held one-to-one was joined from, or `nil`.
        public func joinedWelcome(_ groupIdHex: String) -> String? {
            Self.lock.lock()
            defer { Self.lock.unlock() }
            return read().joined?[groupIdHex]?.welcomeB64
        }

        /// Decided lost: re-check later without reading the journal again.
        @discardableResult
        public func setSettled(_ keyHex: String) -> Bool {
            update { body in
                var own = body.own[keyHex] ?? Own()
                own.settled = true
                body.own[keyHex] = own
                return true
            }
        }

        @discardableResult
        public func setWon(_ keyHex: String, messageId: Int64) -> Bool {
            update { body in
                var own = body.own[keyHex] ?? Own()
                own.won = messageId
                body.own[keyHex] = own
                return true
            }
        }

        public func forgetOwn(_ keyHex: String) {
            _ = update { body in
                guard body.own[keyHex] != nil else { return false }
                body.own[keyHex] = nil
                return true
            }
        }

        /// Forget device: drop the account's record.
        public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
            defaults.removeObject(forKey: storageKey(accountIdB64))
        }
    }
}
