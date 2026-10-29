// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 "Phase 2 revised: silent continuity", R.7 and R.8 - what each
// shared conversation does once a member's account-key reset was ACCEPTED
// (`Contacts/ResetVerdict.swift`), as pure decisions.
//
// iOS mirror of `apps/web/src/chat/reset-repair.ts`; the tables are the same
// and so are the cases in `ResetRepairTests`.
//
// Nothing here has a surface: no copy, no row, no flag (R.0).
//
// The four cases (R.7):
//
//   1. one-to-one: not repairable (a one-to-one cannot lose a person). The
//      surviving side founds a successor by claim-then-build.
//   2. repair: a role-table group the superseded identity is a member or an
//      admin of. Owners and admins propose `Remove` for the old leaves and
//      `Add` for the new identity's devices; the owner restores an admin.
//      The group and the conversation id do not change.
//   3. owned: the superseded identity OWNS the group. Its last leaf cannot be
//      removed and no transfer can name a new owner; a successor is founded
//      by claim-then-build.
//   4. no-role-table: nothing is done.
public enum ResetRepair {

    /// This device's ADR-0022 role in a group; `nil` when it has none (no
    /// role table, or the read failed).
    public enum OwnRole: Equatable {
        case owner, admin, member
    }

    /// Which of R.7's cases a group is, for ONE superseded key in its roster.
    public enum Case: Equatable {
        case oneToOne, repair, owned, noRoleTable
    }

    /// | kind       | role table | owner is the superseded key | case            |
    /// |------------|------------|-----------------------------|-----------------|
    /// | one-to-one | any        | any                         | `oneToOne`      |
    /// | group      | absent     | -                           | `noRoleTable`   |
    /// | group      | present    | yes                         | `owned`         |
    /// | group      | present    | no                          | `repair`        |
    public static func resetCase(isOneToOne: Bool, ownerKeyB64: String?, supersededKeyB64: String) -> Case {
        if isOneToOne { return .oneToOne }
        guard let ownerKeyB64 else { return .noRoleTable }
        return ownerKeyB64 == supersededKeyB64 ? .owned : .repair
    }

    /// A roster leaf as these decisions read it: the leaf and the PERSON key
    /// (the account key the leaf is bound to, ADR-0050), base64.
    public struct PersonLeaf: Equatable {
        public let leafIndex: UInt32
        public let personKeyB64: String

        public init(leafIndex: UInt32, personKeyB64: String) {
            self.leafIndex = leafIndex
            self.personKeyB64 = personKeyB64
        }
    }

    /// The leaves bound to `supersededKeyB64`: the old identity's devices.
    public static func leavesOfKey(_ roster: [PersonLeaf], supersededKeyB64: String) -> [UInt32] {
        roster.filter { $0.personKeyB64 == supersededKeyB64 }.map(\.leafIndex)
    }

    public struct RepairPlan: Equatable {
        public let removeLeaf: UInt32?
        public let mayAdd: Bool
    }

    /// What THIS device proposes in a `repair` group (R.7 case 2), for one
    /// superseded key.
    ///
    /// | own role      | old leaves in roster | proposes                                   |
    /// |---------------|----------------------|--------------------------------------------|
    /// | owner / admin | yes                  | `Remove` of an old leaf not already pending |
    /// | owner / admin | any                  | `Add` of the missing new devices            |
    /// | member / none | any                  | nothing (the core refuses it)               |
    ///
    /// A `Remove` already in the pending set, or this device's own in flight,
    /// is not proposed again; a torn roster (`nil`) proposes nothing.
    public static func repairPlan(
        ownRole: OwnRole?,
        roster: [PersonLeaf]?,
        supersededKeyB64: String,
        pendingRemoveLeaves: [UInt32],
        removeInFlight: Bool,
        membershipEnded: Bool
    ) -> RepairPlan {
        let none = RepairPlan(removeLeaf: nil, mayAdd: false)
        guard !membershipEnded, let roster else { return none }
        guard ownRole == .owner || ownRole == .admin else { return none }
        let candidate = leavesOfKey(roster, supersededKeyB64: supersededKeyB64)
            .first { !pendingRemoveLeaves.contains($0) }
        return RepairPlan(removeLeaf: removeInFlight ? nil : candidate, mayAdd: true)
    }

    /// R.7 case 2 / the re-admission arm: whether this device proposes an
    /// `Add` for the devices of an account whose reset it accepted and whose
    /// NEW key is not in the group yet. Only an owner or an admin: a plain
    /// member's `Add` for an account that is not a member is refused by the
    /// core, so it is not sent.
    public static func mayAddResetIdentity(_ ownRole: OwnRole?) -> Bool {
        ownRole == .owner || ownRole == .admin
    }

    /// One role change the owner proposes next (one per sweep, C-65).
    public enum AdminRestoreStep: Equatable {
        case promote(keyB64: String)
        case demote(keyB64: String)
    }

    /// R.7 case 2: the role table the OWNER proposes when the superseded key
    /// was an admin, as the NEXT single role change (the swap is two steps on
    /// two passes).
    ///
    /// | superseded key is an admin | new key in roster | new key is an admin | a role change is pending | next step       |
    /// |----------------------------|-------------------|---------------------|--------------------------|-----------------|
    /// | any                        | any               | any                 | yes                      | none            |
    /// | no                         | any               | any                 | no                       | none            |
    /// | yes                        | no                | -                   | no                       | none (wait)     |
    /// | yes                        | yes               | no                  | no                       | promote the new |
    /// | yes                        | yes               | yes                 | no                       | demote the old  |
    ///
    /// The new key is promoted BEFORE the old one is demoted, so the person is
    /// never without the role in between; nobody but the owner proposes
    /// either. A role for a key no leaf holds is never written.
    public static func adminRestoreStep(
        ownRole: OwnRole?,
        adminKeysB64: [String],
        supersededKeyB64: String,
        newKeyB64: String,
        rosterPersonKeysB64: [String],
        roleChangePending: Bool
    ) -> AdminRestoreStep? {
        if roleChangePending { return nil }
        guard ownRole == .owner,
              adminKeysB64.contains(supersededKeyB64),
              rosterPersonKeysB64.contains(newKeyB64)
        else { return nil }
        return adminKeysB64.contains(newKeyB64)
            ? .demote(keyB64: supersededKeyB64)
            : .promote(keyB64: newKeyB64)
    }

    /// R.7 case 2 / UX 16.5: the `Members changed` row is suppressed for a
    /// commit that leaves the set of PEOPLE unchanged, each person named by
    /// account id. A torn read on either side suppresses nothing.
    public static func membersChangedSuppressed(peopleBefore: [String]?, peopleAfter: [String]?) -> Bool {
        guard let peopleBefore, let peopleAfter else { return false }
        return Set(peopleBefore) == Set(peopleAfter)
    }

    /// The people of a roster by the name the suppression compares: the
    /// account id a key is known under, else the key itself.
    public static func peopleByAccount(
        _ personKeysB64: [String], accountOfKey: (String) -> String?
    ) -> [String] {
        var out: [String] = []
        for key in personKeysB64 {
            let person = accountOfKey(key) ?? key
            if !out.contains(person) { out.append(person) }
        }
        return out
    }

    /// How long a person whose superseded devices were removed is expected
    /// back before the memory of the first half is dropped. The journal
    /// retention window; chosen, not measured.
    public static let deviceSwapPairWindowMs: Int64 = 30 * 24 * 60 * 60 * 1000

    public struct SwapVerdict: Equatable {
        public let suppress: Bool
        /// account (as ``peopleByAccount(_:accountOfKey:)`` names it) -> when
        /// its superseded devices left.
        public let awaited: [String: Int64]
    }

    /// R.7 case 2 / UX 16.5 across TWO commits. The repair of a member who
    /// reset is a `Remove` of the old leaves and an `Add` of the new devices;
    /// when they land in two commits the first looks like a person leaving
    /// and the second like a person joining, though the set of people across
    /// the PAIR is unchanged. This judges one commit with the memory of the
    /// halves seen before it, and returns the memory to keep.
    ///
    /// The inputs are PERSON keys (ADR-0050 account keys, one per person), so
    /// a commit that only adds or retires DEVICE leaves of the people already
    /// there (a new device admitted, a removed device retired) leaves the key
    /// set equal: the people did not change, and no row is shown for it. (The
    /// table used to keep the row for "changes no key"; written before person
    /// keys, it put three "Members changed" rows into a one-to-one whose two
    /// people never changed.)
    ///
    /// | the commit ...                                                  | row        | memory                 |
    /// |-----------------------------------------------------------------|------------|------------------------|
    /// | changes no person key (devices only)                            | suppressed | unchanged              |
    /// | changes keys, not people (one batch)                            | suppressed | unchanged              |
    /// | only REMOVES people, each present through superseded keys alone | suppressed | those people are added |
    /// | only ADDS people, each one awaited                              | suppressed | those people leave it  |
    /// | anything else                                                   | kept       | whoever came leaves it |
    ///
    /// An awaited person older than ``deviceSwapPairWindowMs`` is forgotten
    /// first. A torn read keeps the row and the memory.
    public static func deviceSwapAcrossCommits(
        personKeysBefore: [String]?,
        personKeysAfter: [String]?,
        accountOfKey: (String) -> String?,
        isSuperseded: (String) -> Bool,
        awaited: [String: Int64],
        nowMs: Int64
    ) -> SwapVerdict {
        var live = awaited.filter { nowMs - $0.value < deviceSwapPairWindowMs }
        guard let before = personKeysBefore, let after = personKeysAfter else {
            return SwapVerdict(suppress: false, awaited: live)
        }
        // No person key changed: only devices did. The people are the same,
        // so there is nothing to tell.
        if Set(before) == Set(after) { return SwapVerdict(suppress: true, awaited: live) }
        let peopleBefore = peopleByAccount(before, accountOfKey: accountOfKey)
        let peopleAfter = peopleByAccount(after, accountOfKey: accountOfKey)
        let gone = peopleBefore.filter { !peopleAfter.contains($0) }
        let came = peopleAfter.filter { !peopleBefore.contains($0) }
        // Whoever is in the roster now is no longer awaited, whatever the verdict.
        for person in peopleAfter { live.removeValue(forKey: person) }
        if gone.isEmpty && came.isEmpty { return SwapVerdict(suppress: true, awaited: live) }
        let goneBySupersededOnly = gone.allSatisfy { person in
            before.filter { (accountOfKey($0) ?? $0) == person }.allSatisfy(isSuperseded)
        }
        let cameAwaited = came.allSatisfy { person in
            guard let since = awaited[person] else { return false }
            return nowMs - since < deviceSwapPairWindowMs
        }
        if came.isEmpty && goneBySupersededOnly {
            for person in gone { live[person] = nowMs }
            return SwapVerdict(suppress: true, awaited: live)
        }
        if gone.isEmpty && cameAwaited { return SwapVerdict(suppress: true, awaited: live) }
        return SwapVerdict(suppress: false, awaited: live)
    }

    // MARK: - The two decisions awaiting the maintainer's confirmation
    //
    // Each is ONE function: flipping the default is a one-line change here
    // and nowhere else.

    /// R.7 case 3: after the successor of an OWNED group is built, is
    /// ownership transferred back to the identity that reset? `true` (the
    /// ADR's default): the founding admin proposes the ownership transfer to
    /// the new identity once it is a member. `false`: the founding admin
    /// stays the owner.
    public static func restoreOwnershipToResetIdentity() -> Bool {
        true
    }

    /// R.7 case 1 / decision D5: does the founder of a successor one-to-one
    /// re-send, into it, its own account's messages sent in the OLD
    /// one-to-one after the attestation's `signed_at` (they reached only
    /// removed devices)? `true` (the ADR's default): re-send. `false`: they
    /// are lost with no sign.
    public static func resendOwnMessagesAfterReset() -> Bool {
        true
    }

    /// A stored message of the old one-to-one, as the re-send reads it.
    public struct OldSegmentMessage: Equatable {
        public let messageId: Int64
        /// Whether THIS account sent it (any of its devices).
        public let own: Bool
        /// The row's time, UNIX milliseconds; `nil` when it carries none.
        public let sentAtMs: Int64?
        /// A chat message with content to send again (not an op, a system
        /// row, a deletion or a call record).
        public let resendable: Bool

        public init(messageId: Int64, own: Bool, sentAtMs: Int64?, resendable: Bool) {
            self.messageId = messageId
            self.own = own
            self.sentAtMs = sentAtMs
            self.resendable = resendable
        }
    }

    /// R.7 case 1: the ids of the old segment's messages the founder sends
    /// again into the successor, in their original order. Empty when
    /// ``resendOwnMessagesAfterReset()`` is off. A row with no readable time
    /// is NOT re-sent: the rule is "after the reset was signed", and an
    /// unknown time proves nothing.
    ///
    /// `afterMessageId` is the durable cursor of an interrupted re-send: the
    /// old journal's id of the last message that landed in the successor.
    /// Only later ids are returned, so a re-send continues and never starts
    /// again from the first message.
    public static func messagesToResend(
        _ messages: [OldSegmentMessage], resetSignedAtMs: Int64, afterMessageId: Int64? = nil
    ) -> [Int64] {
        guard resendOwnMessagesAfterReset() else { return [] }
        let after = afterMessageId ?? 0
        return messages
            .filter { $0.own && $0.resendable && ($0.sentAtMs.map { $0 > resetSignedAtMs } ?? false) }
            .map(\.messageId)
            .filter { $0 > after }
            .sorted()
    }

    /// What a re-sent message carries besides its text and attachments.
    public struct ResendParts: Equatable {
        /// The reply target: `HeldSend.replyNoJumpTarget` (the original the
        /// quote names is a row of the OLD journal), or `nil` for no reply.
        public let replyTo: Int64?
        public let replyAuthor: String?
        public let replyExcerpt: String?
        public let forwardedFrom: String?
        /// The forward's origin, kept as a whole pair or not at all.
        public let forwardOriginGroupB64: String?
        public let forwardOriginMessageId: Int64?
        /// The re-send marker: the ORIGINAL group and message id.
        public let resendOriginGroupB64: String
        public let resendOriginMessageId: Int64
    }

    /// R.7 case 1: a re-sent message is the SAME message. A reply keeps its
    /// quote (the snapshot) with target 0, since the quoted row lives in the
    /// old journal and a successor's ids restart; a forward keeps its
    /// attribution and its origin. A forward never carries a quote
    /// (ADR-0052 section 5.1), so a row holding both is sent as the forward.
    ///
    /// | the original is | reply fields                 | forward fields        |
    /// |-----------------|------------------------------|-----------------------|
    /// | a plain message | none                         | none                  |
    /// | a reply         | target 0 + author + excerpt  | none                  |
    /// | a forward       | none                         | author + whole origin |
    public static func resendParts(
        replyAuthor: String?, replyExcerpt: String?, isReply: Bool,
        forwardAuthor: String?, forwardOriginGroupB64: String?, forwardOriginMessageId: Int64?,
        isForward: Bool,
        originGroupB64: String, originMessageId: Int64
    ) -> ResendParts {
        let origin: (String, Int64)? = {
            guard isForward, let group = forwardOriginGroupB64, !group.isEmpty,
                  let id = forwardOriginMessageId else { return nil }
            return (group, id)
        }()
        let reply = isReply && !isForward
        return ResendParts(
            replyTo: reply ? HeldSend.replyNoJumpTarget : nil,
            replyAuthor: reply ? replyAuthor : nil,
            replyExcerpt: reply ? replyExcerpt : nil,
            forwardedFrom: isForward ? forwardAuthor : nil,
            forwardOriginGroupB64: origin?.0,
            forwardOriginMessageId: origin?.1,
            resendOriginGroupB64: originGroupB64,
            resendOriginMessageId: originMessageId)
    }

    /// R.7 case 3: who the winner adds to an owned group's successor: every
    /// person of the old roster except the superseded keys and this device's
    /// own account (it is the founder), each once.
    public static func successorPeople(
        oldRosterPersonKeysB64: [String], supersededKeysB64: Set<String>, ownPersonKeyB64: String
    ) -> [String] {
        var out: [String] = []
        for key in oldRosterPersonKeysB64 {
            if key == ownPersonKeyB64 || supersededKeysB64.contains(key) || out.contains(key) { continue }
            out.append(key)
        }
        return out
    }

    /// R.7 case 3 "the completeness re-check": the people of the old roster
    /// that the successor still lacks.
    public static func successorMissingPeople(
        oldRosterPersonKeysB64: [String], successorPersonKeysB64: [String], supersededKeysB64: Set<String>
    ) -> [String] {
        let have = Set(successorPersonKeysB64)
        var out: [String] = []
        for key in oldRosterPersonKeysB64 {
            if supersededKeysB64.contains(key) || have.contains(key) || out.contains(key) { continue }
            out.append(key)
        }
        return out
    }

    /// R.7 case 3: the admin set the winner restores in the successor: the
    /// old table's admins that are members of the successor, minus the
    /// superseded keys and minus the successor's owner.
    public static func successorAdmins(
        oldAdminKeysB64: [String], successorPersonKeysB64: [String],
        successorOwnerKeyB64: String, supersededKeysB64: Set<String>
    ) -> [String] {
        let members = Set(successorPersonKeysB64)
        return oldAdminKeysB64.filter {
            members.contains($0) && $0 != successorOwnerKeyB64 && !supersededKeysB64.contains($0)
        }
    }

    /// R.5 "the old segment after the move" / R.8: a row of an old segment
    /// whose MLS-authenticated sender resolves to a SUPERSEDED key is dropped
    /// before storage (a removed device still holding the old keys can post
    /// there). Only for a key an accepted reset retired; a proven predecessor
    /// of a current key is the same person and is never dropped.
    public static func dropsSupersededSender(_ senderKeyB64: String?, supersededKeys: Set<String>) -> Bool {
        senderKeyB64.map { supersededKeys.contains($0) } ?? false
    }

    /// R.8 "nothing from a superseded sender acts": the rows of a received
    /// batch that are kept, given each row's APPLICATION sender.
    ///
    /// | the row is                                              | its sender                | kept |
    /// |---------------------------------------------------------|---------------------------|------|
    /// | an MLS handshake row (a commit, a proposal)             | none                      | yes  |
    /// | any application row: a chat row, an op, a control or    | a key no reset retired    | yes  |
    /// | service envelope, call signaling                        | a key a reset retired     | no   |
    /// | an application row whose leaf did not resolve           | none                      | yes  |
    ///
    /// The rule is on the SENDER, not on the kind, so a kind added later is
    /// covered without being remembered here. The last row is not a hole:
    /// every consumer already refuses an application row it cannot attribute
    /// (an unresolved author edits, pins, rings and names nothing).
    ///
    /// `senders` runs parallel to `rows`; a row past its end has no sender.
    public static func keptAfterSupersededSenders<Row>(
        _ rows: [Row], senders: [String?], supersededKeys: Set<String>
    ) -> [Row] {
        if supersededKeys.isEmpty { return rows }
        var kept: [Row] = []
        kept.reserveCapacity(rows.count)
        for (index, row) in rows.enumerated() {
            let sender = index < senders.count ? senders[index] : nil
            if dropsSupersededSender(sender, supersededKeys: supersededKeys) { continue }
            kept.append(row)
        }
        return kept
    }
}

// MARK: - ADR-0073 R.2: the wider re-admission lookup

extension DeviceReadmission {

    /// One roster person the arm looks up.
    public struct LookupCandidate: Equatable {
        /// The PERSON key the roster names (the key the leaves are bound to).
        public let rosterKeyB64: String
        /// The account id this device knows that person under.
        public let accountId: Data
    }

    /// A contact row as the lookup reads it.
    public struct LookupContact: Equatable {
        public let signaturePubkeyB64: String
        public let previousKeysB64: [String]
        public let accountId: Data?

        public init(signaturePubkeyB64: String, previousKeysB64: [String], accountId: Data?) {
            self.signaturePubkeyB64 = signaturePubkeyB64
            self.previousKeysB64 = previousKeysB64
            self.accountId = accountId
        }
    }

    /// Every roster person, other than this account, whose account id this
    /// device knows: from the contact row holding the roster key (as its
    /// current key or one a reset retired), else from the member's own
    /// profile. The id is a CLAIM in the second case; what it is worth is
    /// decided by ``lookupStanding(rosterKeyB64:pin:proven:binding:)``.
    public static func readmissionCandidates(
        rosterPersonKeysB64: [String],
        ownPersonKeyB64: String,
        contacts: [LookupContact],
        profileAccountIdOf: (String) -> Data?
    ) -> [LookupCandidate] {
        var out: [LookupCandidate] = []
        var seen = Set<String>()
        for rosterKeyB64 in rosterPersonKeysB64 {
            if rosterKeyB64 == ownPersonKeyB64 || seen.contains(rosterKeyB64) { continue }
            seen.insert(rosterKeyB64)
            let row = contacts.first {
                $0.signaturePubkeyB64 == rosterKeyB64 || $0.previousKeysB64.contains(rosterKeyB64)
            }
            guard let accountId = row?.accountId ?? profileAccountIdOf(rosterKeyB64),
                  accountId.count == 16 else { continue }
            out.append(LookupCandidate(rosterKeyB64: rosterKeyB64, accountId: accountId))
        }
        return out
    }

    public enum LookupStanding: Equatable {
        /// The roster key is the account's current key or a PROVEN
        /// predecessor of it: the ordinary ADR-0050 re-admission.
        case samePerson
        /// The roster key was retired by an ACCEPTED RESET of the account:
        /// the directory now lists the new identity's devices (R.7 case 2).
        case resetIdentity
        /// Anything else (no pin, an unrelated key): the account's devices
        /// are not admitted under this person.
        case skip
    }

    /// | pinned | the roster key is ...                          | standing         |
    /// |--------|------------------------------------------------|------------------|
    /// | yes    | the pin, or a proven predecessor of it         | `samePerson`     |
    /// | yes    | a key an accepted reset retired                | `resetIdentity`  |
    /// | yes    | anything else                                  | `skip`           |
    /// | no     | the key of this device's VERIFIED BINDING      | `samePerson`     |
    /// | no     | anything else, or no binding                   | `skip`           |
    ///
    /// `skip` is what stops a member's own profile from attaching another
    /// account's devices to itself: an account id is only followed to devices
    /// when the roster's key is one this device TRUSTS for that account.
    public static func lookupStanding(
        rosterKeyB64: String,
        pin: PeerPinStore.AccountPubkeyPin?,
        provenPredecessorsB64: [String],
        binding: VerifiedBinding?
    ) -> LookupStanding {
        guard let pin else {
            return binding?.keyB64 == rosterKeyB64 ? .samePerson : .skip
        }
        if pin.pinnedSignaturePubkeyB64 == rosterKeyB64 || provenPredecessorsB64.contains(rosterKeyB64) {
            return .samePerson
        }
        return (pin.supersededKeysB64 ?? []).contains(rosterKeyB64) ? .resetIdentity : .skip
    }
}
