// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0060 — the pure rules of "who can find me" on this client: which reach
/// keys the owner must have registered, which received grant may be kept, and
/// which grant authorises a write to someone's mailbox.
///
/// Everything here is a function of its arguments (no network, no store, no
/// clock), so `LakkoTests` pins each rule directly. The derivation and the
/// visibility predicates come from the core over the FFI (`reachSeed`,
/// `allowsScope`, `allowsContactsOfContacts`): this file only decides WHICH
/// keys, never HOW a key is derived.

/// One reach key the owner registers with the Delivery Service (ADR-0060
/// section 1): the class, its scope, its version and an optional expiry.
public struct ReachKeySpec: Hashable {
    public let reachClass: FfiReachClass
    /// Empty for every account-wide class (all but `link` / `reply`; ADR-0078
    /// section 9), the scope id otherwise.
    public let scopeId: Data
    public let version: UInt32
    /// Unix ms after which the DS stops honouring the key; `nil` for keys that
    /// live until removed.
    public let expiresAtMs: UInt64?

    public init(reachClass: FfiReachClass, scopeId: Data, version: UInt32, expiresAtMs: UInt64?) {
        self.reachClass = reachClass
        self.scopeId = scopeId
        self.version = version
        self.expiresAtMs = expiresAtMs
    }
}

/// A live `/i/` or `/c/` link's reach scope (ADR-0060 section 6, "link").
public struct LinkReach: Codable, Hashable {
    public let linkIdB64: String
    public let version: UInt32
    /// `nil` for a link that never expires (the web reach book's `null`).
    public let expiresAtMs: UInt64?
    /// The discovery ledger id the link was uploaded under (base64), when it
    /// was — what a revoke names.
    public var blobIdB64: String?
    /// Which ledger holds it: ``LinkReach/callKind`` for a `/c/` link, `nil`
    /// for an `/i/` link (and for a record stored before the field existed).
    /// A revoke goes to the ledger of its kind.
    public var kind: String?

    /// Not past its expiry; a link without one is always live.
    public func isLive(nowMs: UInt64) -> Bool {
        expiresAtMs.map { $0 > nowMs } ?? true
    }

    /// ``kind`` of a `/c/` call link.
    public static let callKind = "call"

    public init(
        linkIdB64: String, version: UInt32, expiresAtMs: UInt64?, blobIdB64: String? = nil,
        kind: String? = nil
    ) {
        self.linkIdB64 = linkIdB64
        self.version = version
        self.expiresAtMs = expiresAtMs
        self.blobIdB64 = blobIdB64
        self.kind = kind
    }
}

/// One outgoing request's single-use `reply` scope (ADR-0060 section 6): a
/// random 16-byte id, registered with a 7-day expiry before the request goes
/// out, removed once answered.
public struct ReplyReach: Codable, Hashable {
    public let scopeIdB64: String
    public let expiresAtMs: UInt64
    /// What the reply is for, so the right event can retire it: the account id
    /// (base64) of a contact-request recipient, a `/g/` group id (hex), or a
    /// `/c/` call id (hex). Informational; an expired reply is dropped anyway.
    public let purpose: String

    public init(scopeIdB64: String, expiresAtMs: UInt64, purpose: String) {
        self.scopeIdB64 = scopeIdB64
        self.expiresAtMs = expiresAtMs
        self.purpose = purpose
    }
}

/// The purpose strings a `reply` scope is filed under, so the event that
/// answers a request can retire exactly its key.
public enum ReplyPurpose {
    /// A contact request to the account id given.
    public static func contact(_ accountId: Data) -> String { "contact:" + accountId.base64EncodedString() }
    /// A `/g/` join request to the group id given.
    public static func groupJoin(_ groupId: Data) -> String { "group:" + groupId.base64EncodedString() }

    /// The recipient account id of a ``contact(_:)`` purpose, `nil` for any
    /// other purpose.
    public static func contactAccountId(of purpose: String) -> Data? {
        guard purpose.hasPrefix("contact:") else { return nil }
        guard let id = Data(base64Encoded: String(purpose.dropFirst("contact:".count))),
              id.count == 16 else { return nil }
        return id
    }
}

public enum ReachRules {

    /// The version a `(class, scope)` starts at before any rotation: `0`, as
    /// on web and Android (ADR-0060 section 1, "A class with no registered key
    /// starts at 0"). It was `1` here, so one account's iOS and web devices
    /// registered different keys for the same unrotated class, and after one
    /// rotation on web the key web registered was the one iOS had used before.
    public static let initialVersion: UInt32 = 0

    /// ADR-0060 section 1, "Versions after a restore" — how many versions of
    /// one `(class, scope)` a device with no version state for it derives and
    /// looks for among the keys its mailbox holds (versions `0...limit`).
    ///
    /// A version advances once per rotation of that one class: the `contact`
    /// / `fof` keys on each removed contact, the `group` / `member` keys on
    /// each shrink of a group and each group left, `group` / `channel` on each
    /// change of the setting, `channel` on each channel left. 256 rotations of one class
    /// is the bound this client recovers; chosen, not measured. Above it a
    /// restored device finds no registered key and starts the class at 0 like
    /// one that never had any (the stated residual). The cost is up to 257 key
    /// derivations per unknown `(class, scope)`, paid once (the slot is then
    /// known), off the main actor.
    public static let versionRecoveryProbeLimit: UInt32 = 256

    /// A `(class, scope)` whose version is a counter the account's devices
    /// keep, i.e. one ``recoveredVersions(slots:registered:limit:publicKey:)``
    /// can recover: every account-wide class (`contact`, `fof`, `group`,
    /// `member`, `channel`; empty scope, ADR-0078 section 9). Not `link` /
    /// `reply` (one random scope per capability, never rotated).
    public struct VersionSlot: Hashable {
        public let reachClass: FfiReachClass
        public let scopeId: Data

        public init(reachClass: FfiReachClass, scopeId: Data) {
            self.reachClass = reachClass
            self.scopeId = scopeId
        }

        /// The stable text form kept in ``ReachRecord/versionCheckedSlots``.
        public var storageKey: String { ReachRules.className(reachClass) + ":" + scopeId.base64EncodedString() }
    }

    /// Whether `reachClass` carries a recoverable version counter.
    public static func isCounterClass(_ reachClass: FfiReachClass) -> Bool {
        isAccountWide(reachClass)
    }

    /// ADR-0078 section 9 — whether `reachClass` is ONE key per account (an
    /// empty scope): every class but `link` and `reply`. A `group`, `member`
    /// or `channel` key is the same in every group and channel, so a write
    /// under one no longer names the group its writer shares with the owner.
    public static func isAccountWide(_ reachClass: FfiReachClass) -> Bool {
        switch reachClass {
        case .link, .reply: return false
        case .contact, .fof, .group, .member, .channel: return true
        }
    }

    /// The groups and channels (hex ids, sorted) the account was in at the
    /// last registration, so the next one sees which it left (ADR-0078
    /// section 9).
    public struct Membership: Codable, Equatable {
        public var groups: [String]
        public var channels: [String]

        public init(groups: [String], channels: [String]) {
            self.groups = groups
            self.channels = channels
        }
    }

    /// What the account left since the last registration (ADR-0078 section
    /// 9). Pure; the same rule as web's `departuresOf` and Android's.
    public struct Departures: Equatable {
        /// A group the account was in is no longer among its groups.
        public let leftGroup: Bool
        /// A channel the account was in is no longer among its channels.
        public let leftChannel: Bool
        /// The membership to record for the next registration.
        public let membership: Membership
    }

    /// Which classes the account's departures rotate. A group or channel
    /// present at the last registration and absent now was left (or the
    /// account was removed from it): its members still hold this account's
    /// class key, so the class is rotated. An empty current list while the
    /// last one was not is read as "not loaded yet", never as leaving
    /// everything (with no group at all no `group` / `member` key is
    /// registered anyway). `previous == nil` (the first registration) rotates
    /// nothing.
    public static func departures(previous: Membership?, groupIds: [Data], channelIds: [Data]) -> Departures {
        func hexes(_ ids: [Data]) -> [String] {
            Array(Set(ids.map { $0.map { String(format: "%02x", $0) }.joined() })).sorted()
        }
        let groups = hexes(groupIds)
        let channels = hexes(channelIds)
        guard let previous else {
            return Departures(
                leftGroup: false, leftChannel: false, membership: Membership(groups: groups, channels: channels))
        }
        func left(_ before: [String], _ now: [String]) -> Bool {
            !now.isEmpty && before.contains { !now.contains($0) }
        }
        return Departures(
            leftGroup: left(previous.groups, groups),
            leftChannel: left(previous.channels, channels),
            membership: Membership(
                groups: groups.isEmpty ? previous.groups : groups,
                channels: channels.isEmpty ? previous.channels : channels))
    }

    /// The counter slots among `specs` that `state` holds no version for and
    /// that were not checked against the DS yet (`checked`): what must be
    /// recovered before anything is registered.
    public static func uncheckedSlots(
        specs: [ReachKeySpec], state: FfiReachState, checked: Set<String>
    ) -> [VersionSlot] {
        var seen = Set<VersionSlot>()
        var out: [VersionSlot] = []
        for spec in specs where isCounterClass(spec.reachClass) {
            let slot = VersionSlot(reachClass: spec.reachClass, scopeId: spec.scopeId)
            let known = state.entries.contains { $0.class == spec.reachClass && $0.scopeId == spec.scopeId }
            guard !known, !checked.contains(slot.storageKey), seen.insert(slot).inserted else { continue }
            out.append(slot)
        }
        return out
    }

    /// ADR-0060 section 1, "Versions after a restore" — per slot, the HIGHEST
    /// version in `0...limit` whose public key is among `registered` (the keys
    /// the DS holds on this account's mailbox). A slot none of whose versions
    /// is registered is absent from the result: it starts at
    /// ``initialVersion``. Nothing lower than a found version is ever
    /// returned, so a version the owner rotated away is never registered
    /// again.
    public static func recoveredVersions(
        slots: [VersionSlot],
        registered: Set<Data>,
        limit: UInt32 = versionRecoveryProbeLimit,
        publicKey: (VersionSlot, UInt32) -> Data?
    ) -> [VersionSlot: UInt32] {
        var out: [VersionSlot: UInt32] = [:]
        guard !registered.isEmpty else { return out }
        for slot in slots {
            var version: UInt32 = 0
            while true {
                if let pk = publicKey(slot, version), registered.contains(pk) { out[slot] = version }
                if version >= limit { break }
                version += 1
            }
        }
        return out
    }

    /// `state` with each recovered version set (never lowered), stamped
    /// `nowMs` (strictly after the stored stamp) when anything changed.
    public static func applying(
        recovered: [VersionSlot: UInt32], to state: FfiReachState, nowMs: UInt64
    ) -> FfiReachState {
        var entries = state.entries
        var changed = false
        for (slot, version) in recovered.sorted(by: { $0.key.storageKey < $1.key.storageKey }) {
            if let i = entries.firstIndex(where: { $0.class == slot.reachClass && $0.scopeId == slot.scopeId }) {
                guard entries[i].version < version else { continue }
                entries[i] = FfiReachStateEntry(class: slot.reachClass, scopeId: slot.scopeId, version: version)
            } else {
                entries.append(FfiReachStateEntry(class: slot.reachClass, scopeId: slot.scopeId, version: version))
            }
            changed = true
        }
        guard changed else { return state }
        return FfiReachState(entries: entries, updatedAtMs: max(nowMs, state.updatedAtMs &+ 1))
    }

    /// ADR-0060 section 6 — a `reply` key lives 7 days (chosen, not measured).
    public static let replyLifetimeMs: UInt64 = 7 * 24 * 60 * 60 * 1000

    /// ADR-0060 section 2 — a delegation lives at most 24 hours; a proposer
    /// issues it for less so a re-send is due well before it lapses.
    public static let delegationLifetimeMs: UInt64 = 20 * 60 * 60 * 1000

    /// The version of `(class, scope)` in `state`, or ``initialVersion``. An
    /// account-wide class ignores `scope` (ADR-0078 section 9).
    public static func version(
        of reachClass: FfiReachClass, scope: Data, in state: FfiReachState
    ) -> UInt32 {
        let scope = isAccountWide(reachClass) ? Data() : scope
        for entry in state.entries where entry.class == reachClass && entry.scopeId == scope {
            return entry.version
        }
        return initialVersion
    }

    /// `state` with `(class, scope)` bumped by one (a rotation), stamped
    /// `nowMs` so the ADR-0044 strictly-newer order keeps it over an older copy.
    public static func bumping(
        _ reachClass: FfiReachClass, scope: Data, in state: FfiReachState, nowMs: UInt64
    ) -> FfiReachState {
        let scope = isAccountWide(reachClass) ? Data() : scope
        let next = version(of: reachClass, scope: scope, in: state) &+ 1
        var entries = state.entries.filter { !($0.class == reachClass && $0.scopeId == scope) }
        entries.append(FfiReachStateEntry(class: reachClass, scopeId: scope, version: next))
        return FfiReachState(entries: entries, updatedAtMs: max(nowMs, state.updatedAtMs &+ 1))
    }

    /// The group scope of a group id, as the setting names it.
    public static func groupScope(_ groupId: Data) -> FfiScopeId {
        FfiScopeId(kind: .group, id: groupId)
    }

    /// Every reach key the owner must have registered right now (ADR-0060
    /// sections 1 and 6; ADR-0078 section 9).
    ///
    /// - `contact`: always — every contact holds it.
    /// - `member` while the account is in any group, whatever the setting (a
    ///   co-member must always be able to deliver a Welcome for the owner's new
    ///   device).
    /// - `group` while the setting lets the members of any group find the
    ///   owner (`allowsScope`).
    /// - `channel` while the setting lets the members of any channel find it.
    /// - `link` per live link, `reply` per unexpired reply.
    /// - `fof` while contacts of contacts may find the owner.
    ///
    /// `group`, `member` and `channel` are ONE key each for the account, never
    /// one per group or channel. Expired links and replies are left out: the
    /// DS drops them anyway, and leaving them out makes the next sync
    /// unregister them.
    public static func desiredKeys(
        setting: FfiVisibilitySetting,
        state: FfiReachState,
        groupIds: [Data],
        links: [LinkReach],
        replies: [ReplyReach],
        channelIds: [Data] = [],
        nowMs: UInt64,
        allowsScope: (FfiVisibilitySetting, FfiScopeId) -> Bool,
        allowsFof: (FfiVisibilitySetting) -> Bool
    ) -> [ReachKeySpec] {
        var out: [ReachKeySpec] = []
        func accountWide(_ reachClass: FfiReachClass) -> ReachKeySpec {
            ReachKeySpec(
                reachClass: reachClass, scopeId: Data(),
                version: version(of: reachClass, scope: Data(), in: state), expiresAtMs: nil)
        }
        out.append(accountWide(.contact))
        let groups = groupIds.filter { !$0.isEmpty && $0.count <= 64 }
        if !groups.isEmpty {
            out.append(accountWide(.member))
        }
        if groups.contains(where: { allowsScope(setting, groupScope($0)) }) {
            out.append(accountWide(.group))
        }
        for link in links where link.isLive(nowMs: nowMs) {
            guard let id = Data(base64Encoded: link.linkIdB64), !id.isEmpty else { continue }
            out.append(ReachKeySpec(
                reachClass: .link, scopeId: id, version: link.version,
                expiresAtMs: link.expiresAtMs))
        }
        for reply in replies where reply.expiresAtMs > nowMs {
            guard let id = Data(base64Encoded: reply.scopeIdB64), !id.isEmpty else { continue }
            out.append(ReachKeySpec(
                reachClass: .reply, scopeId: id, version: initialVersion,
                expiresAtMs: reply.expiresAtMs))
        }
        if channelIds.contains(where: {
            !$0.isEmpty && $0.count <= 64 && allowsScope(setting, FfiScopeId(kind: .channel, id: $0))
        }) {
            out.append(accountWide(.channel))
        }
        if allowsFof(setting) {
            out.append(accountWide(.fof))
        }
        return out
    }

    /// The registration update that moves the DS from `registered` (the public
    /// keys this device last registered) to `desired` (public key per spec):
    /// every desired key is (re-)added — idempotent, and it refreshes an
    /// expiry — and every registered key no longer desired is removed.
    public static func registrationDiff(
        desired: [(publicKey: Data, expiresAtMs: UInt64?)],
        registered: Set<Data>
    ) -> (add: [DsReachKeyAdd], remove: [Data]) {
        let desiredKeys = Set(desired.map(\.publicKey))
        var seen = Set<Data>()
        var add: [DsReachKeyAdd] = []
        for entry in desired where seen.insert(entry.publicKey).inserted {
            add.append(DsReachKeyAdd(publicKey: entry.publicKey, expiresAtMs: entry.expiresAtMs))
        }
        let remove = registered.subtracting(desiredKeys).sorted { $0.lexicographicallyPrecedes($1) }
        return (add, remove)
    }

    /// `open` is set only in Everyone (ADR-0060 section 2).
    public static func mailboxOpen(_ setting: FfiVisibilitySetting) -> Bool {
        setting.mode == .everyone
    }

    // MARK: - Received grants

    /// Where a received grant arrived (ADR-0060 section 6, "A received
    /// `contact` grant is kept only from the other participant of a 1:1
    /// conversation; `group` / `member` only from members of that group;
    /// `channel` only from that channel's member cards").
    public enum Carrier: Equatable {
        /// A profile envelope in a one-to-one conversation; `fromOtherParticipant`
        /// is whether its sender (resolved at receipt) is the one other person.
        case oneToOneProfile(fromOtherParticipant: Bool)
        /// A profile envelope in a group, from a member of that group.
        case groupProfile
        /// A channel member card.
        case channelMemberCard
        /// A contact request's sealed plaintext, or a `/g/` join attribution.
        case request
    }

    /// Whether `grant` may be kept when it arrived through `carrier`, from a
    /// sender whose identity was resolved at receipt (`senderResolved`, C-68).
    /// An unresolved sender keeps nothing: the grant must be filed under
    /// exactly the key that authenticated it.
    public static func keeps(
        _ grant: FfiReachGrant, carrier: Carrier, senderResolved: Bool
    ) -> Bool {
        guard senderResolved, grant.seed.count == 32 else { return false }
        switch (grant.class, carrier) {
        case (.contact, .oneToOneProfile(let fromOther)):
            return fromOther
        case (.member, .oneToOneProfile(let fromOther)):
            return fromOther
        case (.group, .groupProfile), (.member, .groupProfile):
            return true
        case (.channel, .channelMemberCard):
            return true
        case (.reply, .request):
            return true
        default:
            // A link grant is used once by the link holder and never kept; a
            // fof grant is read from an introduction record, never a carrier;
            // every other pairing is a grant arriving where it must not.
            return false
        }
    }

    // MARK: - Choosing a write authorisation

    /// A grant this device holds for one person, with the scope it was given
    /// in (the group id for `group` / `member`, the channel id for `channel`).
    public struct HeldGrant: Codable, Equatable {
        public let classRaw: String
        public let version: UInt32
        public let seedB64: String
        /// Hex group or channel id, `nil` for `contact` / `reply`.
        public let scopeHex: String?
        public let storedAtMs: UInt64
        /// ADR-0074 section 3.5a rule 6 — `true` for the grant a newer one of
        /// the same class, scope and VERSION displaced: the person's account
        /// seed changed (a key rotation), so this one opens the mailbox of
        /// their old key and the newer one the mailbox of their new key. Kept
        /// beside the newer one, and tried after it, until this device knows
        /// the person's current address. `nil` (not superseded) for a record
        /// stored before the field existed.
        public var superseded: Bool?
        /// ADR-0078 — the owner's 97-byte mailbox binding the grant carried:
        /// where a write signed with this grant goes, once verified against
        /// the owner's account key. `nil` for a record stored before the
        /// field existed; such a grant names no mailbox and is never written
        /// under.
        public var bindingB64: String?

        public init(grant: FfiReachGrant, scopeHex: String?, storedAtMs: UInt64) {
            self.classRaw = ReachRules.className(grant.class)
            self.version = grant.version
            self.seedB64 = grant.seed.base64EncodedString()
            self.scopeHex = scopeHex
            self.storedAtMs = storedAtMs
            self.bindingB64 = grant.binding.base64EncodedString()
        }

        public var isSuperseded: Bool { superseded == true }

        public var reachClass: FfiReachClass? { ReachRules.reachClass(named: classRaw) }
        public var seed: Data? { Data(base64Encoded: seedB64) }
        public var binding: Data? { bindingB64.flatMap { Data(base64Encoded: $0) } }

        /// The grant as the core takes it (for the binding check), or `nil`
        /// when a field does not decode.
        public var ffiGrant: FfiReachGrant? {
            guard let reachClass, let seed, let binding else { return nil }
            return FfiReachGrant(class: reachClass, version: version, seed: seed, binding: binding)
        }
    }

    /// ADR-0078 — the grants held for one person grouped by the mailbox they
    /// write to, best mailbox first. `verify` answers the mailbox a grant's
    /// binding names once it verified against one of the person's account
    /// keys (`nil` refuses the grant: it is never written under). The
    /// mailbox of the grant ``writeSeeds(from:groupIdHex:)`` prefers comes
    /// first; within a mailbox the seeds keep that order.
    public static func writeMailboxes(
        from held: [HeldGrant], groupIdHex: String?, verify: (HeldGrant) -> MailboxTarget?
    ) -> [(target: MailboxTarget, seeds: [Data])] {
        var verified: [(grant: HeldGrant, target: MailboxTarget)] = []
        for grant in held {
            guard let target = verify(grant) else { continue }
            verified.append((grant, target))
        }
        var order: [MailboxTarget] = []
        for seed in writeSeeds(from: verified.map(\.grant), groupIdHex: groupIdHex) {
            guard let entry = verified.first(where: { $0.grant.seed == seed }),
                  !order.contains(entry.target) else { continue }
            order.append(entry.target)
        }
        return order.map { target in
            let grants = verified.filter { $0.target == target }.map(\.grant)
            return (target, writeSeeds(from: grants, groupIdHex: groupIdHex))
        }
    }

    /// The seed to sign a write to a person's mailbox with, from the grants
    /// held for them: `contact` first (the steadiest), then `member` of the
    /// group the write concerns, then any `member` / `group`, then a `reply`.
    /// `nil` when nothing is held — the write then goes unsigned, which only an
    /// open (Everyone) mailbox stores.
    public static func writeSeed(from held: [HeldGrant], groupIdHex: String?) -> Data? {
        writeSeeds(from: held, groupIdHex: groupIdHex).first
    }

    /// Every distinct seed held for one person, in the order ``writeSeed(from:groupIdHex:)``
    /// prefers them: `contact`, then `member` of the group the write concerns,
    /// then any `member`, `group`, `channel`, `fof`, `reply` — newest first within a
    /// class. A Welcome writer tries them in this order and moves to the next
    /// on a `401` (ADR-0050 2.5a item 7): one key the owner rotated away must
    /// not cost a Welcome another key still opens.
    public static func writeSeeds(from held: [HeldGrant], groupIdHex: String?) -> [Data] {
        func ordered(_ held: [HeldGrant]) -> [Data] {
            func all(_ cls: FfiReachClass, scope: String?? = nil) -> [Data] {
                held.filter { $0.reachClass == cls && (scope == nil || $0.scopeHex == scope!) }
                    .sorted { $0.storedAtMs > $1.storedAtMs }
                    .compactMap(\.seed)
            }
            var out: [Data] = all(.contact)
            if let groupIdHex { out += all(.member, scope: .some(groupIdHex)) }
            out += all(.member)
            out += all(.group)
            out += all(.channel)
            // ADR-0060 section 6 — a `fof` grant is held only when this
            // device added a person the search found through it.
            out += all(.fof)
            out += all(.reply)
            return out
        }
        // ADR-0074 section 3.5a rule 6 — a grant a rotation displaced comes
        // after every current one: it opens only the old mailbox.
        let ordered = ordered(held.filter { !$0.isSuperseded }) + ordered(held.filter(\.isSuperseded))
        var seen = Set<Data>()
        return ordered.filter { $0.count == 32 && seen.insert($0).inserted }
    }

    /// Replace the grants held for one person with `incoming` of the same
    /// class and scope — the newest wins — keeping every other.
    ///
    /// ADR-0074 section 3.5a rule 6 — one exception: a grant displaced by one
    /// of the SAME version and another seed is kept beside it, marked
    /// ``HeldGrant/superseded``. A reach key the owner rotated bumps the
    /// version; the same version under another seed means the owner's account
    /// seed changed (a key rotation), so the two open two mailboxes: the old
    /// key's and the new key's. A device that received the re-sent profile
    /// before it observed the proven key still addresses the old mailbox, and
    /// the displaced grant is what opens it. At most one superseded grant per
    /// class and scope; ``droppingSuperseded(_:)`` removes them once the
    /// person's current address is known.
    public static func merging(_ incoming: HeldGrant, into held: [HeldGrant]) -> [HeldGrant] {
        func sameSlot(_ g: HeldGrant) -> Bool {
            g.classRaw == incoming.classRaw && g.scopeHex == incoming.scopeHex
        }
        // A replayed older version never displaces a newer one of the same
        // class and scope (a re-sent profile carries whatever it carried).
        if held.contains(where: { sameSlot($0) && !$0.isSuperseded && $0.version > incoming.version }) {
            return held
        }
        // The same grant again (a re-sent profile): nothing moves, and a
        // superseded grant is never revived by a replay of itself.
        if held.contains(where: {
            sameSlot($0) && $0.seedB64 == incoming.seedB64 && $0.bindingB64 == incoming.bindingB64
        }) {
            return held
        }
        let displaced = held.first { sameSlot($0) && !$0.isSuperseded }
        var out = held.filter { !sameSlot($0) }
        if var displaced, displaced.version == incoming.version {
            displaced.superseded = true
            out.append(displaced)
        } else if let kept = held.first(where: { sameSlot($0) && $0.isSuperseded }),
                  displaced == nil {
            out.append(kept)
        }
        var fresh = incoming
        fresh.superseded = nil
        out.append(fresh)
        return out
    }

    /// ADR-0074 section 3.5a rule 5 — what a key rotation spends: every live
    /// link scope and every outstanding reply scope joins the spent lists
    /// (once each), and none stays live.
    public static func spending(
        links: [LinkReach], replies: [ReplyReach], spentLinks: [LinkReach], spentReplies: [ReplyReach]
    ) -> (links: [LinkReach], replies: [ReplyReach]) {
        var outLinks = spentLinks
        for link in links where !outLinks.contains(where: { $0.linkIdB64 == link.linkIdB64 }) {
            outLinks.append(link)
        }
        var outReplies = spentReplies
        for reply in replies where !outReplies.contains(where: { $0.scopeIdB64 == reply.scopeIdB64 }) {
            outReplies.append(reply)
        }
        return (outLinks, outReplies)
    }

    /// The grants held for one person without the ones a rotation displaced:
    /// what is held once the person's current address is known (the write
    /// goes to the new mailbox, which the displaced grant never opens).
    public static func droppingSuperseded(_ held: [HeldGrant]) -> [HeldGrant] {
        held.filter { !$0.isSuperseded }
    }

    /// ADR-0074 section 3.5a, proposer / committer — which of this account's
    /// own keys signs a delegated write: the first (current key first, then
    /// its own proven predecessors) a delegation in the envelope names. A
    /// proposer that has not observed this account's rotation names the old
    /// key; the write is then signed with that key's seed.
    public static func delegationSigner<Seed>(
        ownKeys: [(key: Data, seed: Seed)], header: (Data) -> String?
    ) -> (seed: Seed, header: String)? {
        for own in ownKeys {
            if let header = header(own.key) { return (own.seed, header) }
        }
        return nil
    }

    public static func className(_ c: FfiReachClass) -> String {
        switch c {
        case .contact: return "contact"
        case .link: return "link"
        case .reply: return "reply"
        case .group: return "group"
        case .member: return "member"
        case .channel: return "channel"
        case .fof: return "fof"
        }
    }

    public static func reachClass(named name: String) -> FfiReachClass? {
        switch name {
        case "contact": return .contact
        case "link": return .link
        case "reply": return .reply
        case "group": return .group
        case "member": return .member
        case "channel": return .channel
        case "fof": return .fof
        default: return nil
        }
    }

    /// Did the group LOSE a member between two roster snapshots? A set
    /// difference over member keys, never a count (ADR-0043); an unreadable
    /// snapshot is never evidence of a shrink.
    public static func membershipShrank(previous: [String]?, current: [String]?) -> Bool {
        guard let previous, let current, !current.isEmpty else { return false }
        return !Set(previous).subtracting(current).isEmpty
    }

    // MARK: - Welcome targets

    /// ADR-0060 section 6, "One inbound mailbox per person" — a Welcome is
    /// addressed to each added leaf's ACCOUNT mailbox, one write per account.
    /// `resolve` maps an added leaf key to its account key (`nil` when this
    /// device cannot resolve it, in which case the leaf key itself is kept so a
    /// later resolution can still find it). Order is kept; duplicates go.
    public static func welcomeTargets(addedLeafKeys: [Data], resolve: (Data) -> Data?) -> [Data] {
        var seen = Set<Data>()
        var out: [Data] = []
        for leaf in addedLeafKeys {
            let target = resolve(leaf) ?? leaf
            if seen.insert(target).inserted { out.append(target) }
        }
        return out
    }
}

/// ADR-0060 section 6 — which committer-delegation envelope an add posts:
/// the one signed with a grant this device holds for the added person when
/// there is one; else, for a person found in Everyone, the one with no
/// delegation naming their open mailbox; else none.
public enum CommitterDelegationChoice {
    public static func envelope(signed: Data?, open: Data?) -> Data? {
        signed ?? open
    }
}
