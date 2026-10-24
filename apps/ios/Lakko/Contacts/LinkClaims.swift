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
        /// The link-founded note was published into the account's
        /// DeviceGroup; no Welcome is written before it.
        public var notePosted: Bool
        public var delivered: Bool

        public init(
            groupIdB64: String, welcomeB64: String, targets: [FoundedTarget],
            notePosted: Bool = false, delivered: Bool
        ) {
            self.groupIdB64 = groupIdB64
            self.welcomeB64 = welcomeB64
            self.targets = targets
            self.notePosted = notePosted
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
        /// A one-to-one was founded for it here already: carry that one on.
        case resume
        /// Another device of the account founded it (its note is held): never
        /// found; write the note's Welcome to the requester.
        case forward
        /// Found the one-to-one.
        case found
    }

    /// Before any device builds a one-to-one for a request (the first winner,
    /// or a device that took it over because the winner left the roster): its
    /// own founding first, else a note from another device, else found.
    public static func buildStep(hasFounded: Bool, hasNote: Bool) -> BuildStep {
        if hasFounded { return .resume }
        return hasNote ? .forward : .found
    }

    /// A link-founded note another device of the account posted (ADR-0079):
    /// the one-to-one it founded for a request, and that group's Welcome.
    public struct HeldNote: Equatable, Codable {
        /// The DS message id of the note's row: the earliest per request is
        /// kept.
        public let id: Int64
        public let groupIdB64: String
        public let welcomeB64: String

        public init(id: Int64, groupIdB64: String, welcomeB64: String) {
            self.id = id
            self.groupIdB64 = groupIdB64
            self.welcomeB64 = welcomeB64
        }
    }

    /// What the build needs from the device, injected so the order is one
    /// tested function (``runBuild(_:)``).
    public struct BuildDeps {
        /// This device's founding for the request, if any.
        public var founded: () -> Founded?
        /// The earliest note another device posted for the request, if any.
        public var note: () -> HeldNote?
        /// Found the one-to-one locally (ONE commit with every other own
        /// device, the journal claimed, the state persisted); nothing is sent.
        public var prepare: () async throws -> Founded
        /// Record the founding durably; `false` when the write failed.
        public var record: (Founded) -> Bool
        /// Publish the link-founded note into the account's DeviceGroup (once:
        /// a sealed note already recorded is not published again), read the
        /// journal past it, and answer whether its row COUNTS (sealed at the
        /// journal's epoch at its position, so every device can read it).
        /// `false` when it was straddled (posted again next pass) or not yet
        /// read.
        public var postNote: (Founded) async throws -> Bool
        /// Make the founded group a held conversation; its hex id, or `nil`.
        public var hold: (Founded) async -> String?
        /// Write the Welcome: this account's mailbox first, then the
        /// requester's.
        public var deliver: (Founded) async throws -> Void
        /// Write a note's Welcome to the requester's mailbox (a duplicate is
        /// ignored); the group's hex id when this device holds it, else `nil`.
        public var forward: (HeldNote) async throws -> String?
        /// The contact row, the ack and the use count.
        public var finish: (String?) async -> Void

        public init(
            founded: @escaping () -> Founded?,
            note: @escaping () -> HeldNote?,
            prepare: @escaping () async throws -> Founded,
            record: @escaping (Founded) -> Bool,
            postNote: @escaping (Founded) async throws -> Bool,
            hold: @escaping (Founded) async -> String?,
            deliver: @escaping (Founded) async throws -> Void,
            forward: @escaping (HeldNote) async throws -> String?,
            finish: @escaping (String?) async -> Void
        ) {
            self.founded = founded
            self.note = note
            self.prepare = prepare
            self.record = record
            self.postNote = postNote
            self.hold = hold
            self.deliver = deliver
            self.forward = forward
            self.finish = finish
        }
    }

    /// The build of a won request, in its one order (ADR-0079, 2026-10-17):
    /// a founding of this device's own is carried on (nothing is founded
    /// twice); else a note from another device means it founded already, so
    /// its Welcome is written to the requester and the request answered,
    /// never founded; else found, record the founding, publish the note, and
    /// only after the note landed counting (its row sealed at the journal's
    /// epoch there, so every device can read it) write the Welcome (own
    /// mailbox first). `false` when a step did not complete (a throw included): every
    /// record stays, for the next pass.
    @MainActor
    public static func runBuild(_ d: BuildDeps) async -> Bool {
        do {
            var founded = d.founded()
            switch buildStep(hasFounded: founded != nil, hasNote: founded == nil && d.note() != nil) {
            case .forward:
                guard let note = d.note() else { return false }
                await d.finish(try await d.forward(note))
                return true
            case .found:
                let made = try await d.prepare()
                guard d.record(made) else { return false }
                founded = made
            case .resume:
                break
            }
            guard var current = founded else { return false }
            if !current.notePosted {
                // No Welcome on the strength of a note the other devices
                // cannot read.
                guard try await d.postNote(current) else { return false }
                current.notePosted = true
                guard d.record(current) else { return false }
            }
            guard let held = await d.hold(current) else { return false }
            if !current.delivered {
                try await d.deliver(current)
                current.delivered = true
                current.welcomeB64 = ""
                _ = d.record(current)
            }
            await d.finish(held)
            return true
        } catch {
            return false
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
        /// The sealed link-founded note (hex), recorded BEFORE its publish
        /// and not yet judged at its journal row.
        public var noteWire: String?
        /// The note's row counted: sealed at the journal's epoch at its
        /// position, so every device of the account can read it.
        public var noteConfirmed: Bool = false

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
        /// The synced state is not loaded yet, or this device holds no record
        /// of the link: keep it undecided, unshown.
        case wait
        /// Answered (by another device, or the register says so): remove.
        case drop
        /// The link does not count for the request (ADR-0079 amendment of
        /// 2026-10-19): drop it silently and answer the requester with a link
        /// refusal. No card.
        case refuse
        /// Live link: post this device's claim.
        case postClaim
        /// Claim posted: read the journal past it and decide.
        case decide
        /// Lost: decide again against the current roster (no journal read).
        case recheck
        /// Claim won: found the one-to-one (again, after a failure).
        case build
        /// The requester already has a live one-to-one with this account: put
        /// them back on the contact list with that conversation, found
        /// nothing, send nothing (a person removed and invited again; amended
        /// 2026-10-24).
        case restore
    }

    /// The next step for one request.
    ///
    /// | own claims        | answered elsewhere | verdict          | step        |
    /// |-------------------|--------------------|------------------|-------------|
    /// | won, not founded, no note held | any   | dead             | `refuse`    |
    /// | won               | any                | any other        | `build`     |
    /// | any other         | any                | answered         | `drop`      |
    /// | lost (settled)    | any                | dead             | `drop`      |
    /// | lost (settled)    | any                | other            | `recheck`   |
    /// | posted or posting | any                | any              | `decide`    |
    /// | none              | yes                | dead             | `drop`      |
    /// | none              | yes                | other            | `wait`      |
    /// | none              | no                 | not held/unknown | `wait`      |
    /// | none              | no                 | dead             | `refuse`    |
    /// | none              | no                 | accept           | `postClaim` |
    ///
    /// Before the rows after the first: a requester this device already holds
    /// a live one-to-one with is answered by `restore` whenever the link
    /// counts (`accept`) or was already answered for them (`answered`). Every
    /// device of the account restores on its own; nothing is founded, so no
    /// claim decides between them. A claim this device already won keeps
    /// building.
    ///
    /// "Answered elsewhere" is the core's `linkClaimAnsweredElsewhere`: a
    /// claim from a device in the current roster. The verdict judges the
    /// link's expiry at the moment the server stamped the request and its
    /// revocation now, so a revocation wins until this account has answered
    /// (ADR-0079 amendment of 2026-10-19): a winner that has not founded yet,
    /// and holds no other device's founding note, refuses instead of
    /// building. A device that lost, or sees another device's claim, drops a
    /// dead request without a refusal: the claimer answers it. A link this
    /// device holds no record of waits. Mirror of web `linkRequestStep`.
    public static func step(
        own: Own?, answeredElsewhere: Bool, verdict: LinkAnswerVerdictValue?, noteHeld: Bool = false,
        liveOneToOne: Bool = false
    ) -> Step {
        if let own, own.holdsClaim, own.won != nil {
            let answered = own.founded != nil || noteHeld
            return verdict == .dead && !answered ? .refuse : .build
        }
        if liveOneToOne, verdict == .accept || verdict == .answered { return .restore }
        if verdict == .answered { return .drop }
        if let own, own.holdsClaim {
            if !own.settled { return .decide }
            return verdict == .dead ? .drop : .recheck
        }
        if answeredElsewhere { return verdict == .dead ? .drop : .wait }
        guard let verdict else { return .wait }
        switch verdict {
        case .answered: return .drop
        case .unknown: return .wait
        case .dead: return .refuse
        case .accept: return .postClaim
        }
    }

    /// The claims the core decides over: this device's own (authored by its
    /// own leaf key) and the others seen.
    public static func claimsToDecide(own: Own?, seen: [Observed], ownKeyHex: String) -> [Observed] {
        (own?.ids ?? []).map { Observed(id: $0, author: ownKeyHex) } + seen
    }

    /// The register's verdict as the rule reads it (the core's
    /// `LinkAnswerVerdict`, kept separate so the rule is testable without
    /// the binding).
    public enum LinkAnswerVerdictValue: Equatable { case accept, answered, dead, unknown }

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

        private struct StoredNote: Codable, Equatable {
            var note: HeldNote
            var at: Int64
        }

        /// The journal's epochs as this device read them (ADR-0079): the
        /// persisted form of the core's `JournalEpochs`, for one DeviceGroup.
        public struct StoredJournal: Codable, Equatable {
            public let groupIdHex: String
            public let base: UInt64
            public let commitRowIds: [UInt64]
            public let commitEpochsAfter: [UInt64]

            public init(groupIdHex: String, base: UInt64, commitRowIds: [UInt64], commitEpochsAfter: [UInt64]) {
                self.groupIdHex = groupIdHex
                self.base = base
                self.commitRowIds = commitRowIds
                self.commitEpochsAfter = commitEpochsAfter
            }
        }

        private struct Body: Codable, Equatable {
            var v: Int = 2
            var journal: StoredJournal?
            var seen: [String: Seen] = [:]
            var own: [String: Own] = [:]
            /// The earliest link-founded note per request, by request key.
            var notes: [String: StoredNote]? = [:]
        }

        /// How many link-founded notes are kept; past it the oldest go.
        /// Chosen, not measured (the web value).
        public static let maxNotes = 64

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

        /// A journal row carried these sealed bytes at this id, and `counts`
        /// is the readability rule's answer for it (sealed at the journal's
        /// epoch at its position). A claim of this device: filed as its own
        /// when it counts, else dropped (straddled: the next pass claims
        /// again). This device's note: confirmed when it counts, else dropped
        /// (posted again). Returns `"claim"`, `"note"`, or `nil`.
        @discardableResult
        public func judgeOwnRow(wireHex: String, messageId: Int64, counts: Bool) -> String? {
            var kind: String?
            _ = update { body in
                if let key = body.own.first(where: { $0.value.wires.contains(wireHex) })?.key,
                   var own = body.own[key] {
                    own.wires.removeAll { $0 == wireHex }
                    if counts, !own.ids.contains(messageId) {
                        own.ids.append(messageId)
                        own.ids.sort()
                    }
                    body.own[key] = own
                    kind = "claim"
                    return true
                }
                if let key = body.own.first(where: { $0.value.noteWire == wireHex })?.key,
                   var own = body.own[key] {
                    own.noteWire = nil
                    own.noteConfirmed = own.noteConfirmed || counts
                    body.own[key] = own
                    kind = "note"
                    return true
                }
                return false
            }
            return kind
        }

        /// `true` when some sealed claim or note still waits for its row.
        public func hasUnmatchedWires() -> Bool {
            Self.lock.lock()
            defer { Self.lock.unlock() }
            return read().own.values.contains { !$0.wires.isEmpty || $0.noteWire != nil }
        }

        /// Record the sealed note BEFORE its publish. `false` when the write
        /// failed.
        @discardableResult
        public func setNoteWire(_ keyHex: String, wireHex: String) -> Bool {
            update { body in
                var own = body.own[keyHex] ?? Own()
                own.noteWire = wireHex
                body.own[keyHex] = own
                return true
            }
        }

        /// After a read of the journal past the note: a sealed note no row
        /// carried did not land; drop it (posted again).
        public func dropUnlandedNote(_ keyHex: String) {
            _ = update { body in
                guard var own = body.own[keyHex], own.noteWire != nil else { return false }
                own.noteWire = nil
                body.own[keyHex] = own
                return true
            }
        }

        /// The journal's epochs held for the DeviceGroup `groupIdHex`, or
        /// `nil` (none held, or held for another DeviceGroup).
        public func journal(groupIdHex: String) -> StoredJournal? {
            Self.lock.lock()
            defer { Self.lock.unlock() }
            guard let j = read().journal, j.groupIdHex == groupIdHex else { return nil }
            return j
        }

        /// Keep the journal's epochs.
        @discardableResult
        public func setJournal(_ journal: StoredJournal) -> Bool {
            update { body in
                body.journal = journal
                return true
            }
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

        /// Keep a link-founded note another device posted (the earliest per
        /// request). `true` when the record changed.
        @discardableResult
        public func recordNote(_ keyHex: String, note: HeldNote, nowMs: Int64) -> Bool {
            update { body in
                var notes = body.notes ?? [:]
                if let held = notes[keyHex], held.note.id <= note.id { return false }
                notes[keyHex] = StoredNote(note: note, at: nowMs)
                if notes.count > Self.maxNotes {
                    let oldest = notes.sorted { $0.value.at < $1.value.at }
                    for (k, _) in oldest.prefix(notes.count - Self.maxNotes) { notes[k] = nil }
                }
                body.notes = notes
                return true
            }
        }

        /// The earliest note held for the request, or `nil`.
        public func note(_ keyHex: String) -> HeldNote? {
            Self.lock.lock()
            defer { Self.lock.unlock() }
            return read().notes?[keyHex]?.note
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
