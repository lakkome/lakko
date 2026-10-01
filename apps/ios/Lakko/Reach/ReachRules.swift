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
    /// Empty for `contact` / `fof`, the scope id otherwise.
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
    public let expiresAtMs: UInt64
    /// The discovery ledger id the link was uploaded under (base64), when it
    /// was — what a revoke names.
    public var blobIdB64: String?

    public init(linkIdB64: String, version: UInt32, expiresAtMs: UInt64, blobIdB64: String? = nil) {
        self.linkIdB64 = linkIdB64
        self.version = version
        self.expiresAtMs = expiresAtMs
        self.blobIdB64 = blobIdB64
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
    /// A version advances once per rotation of that one `(class, scope)`: the
    /// `contact` / `fof` keys on each removed contact, a group's `group` /
    /// `member` keys on each shrink of that group. 256 rotations of one scope
    /// is the bound this client recovers; chosen, not measured. Above it a
    /// restored device finds no registered key and starts the class at 0 like
    /// one that never had any (the stated residual). The cost is up to 257 key
    /// derivations per unknown `(class, scope)`, paid once (the slot is then
    /// known), off the main actor.
    public static let versionRecoveryProbeLimit: UInt32 = 256

    /// A `(class, scope)` whose version is a counter the account's devices
    /// keep, i.e. one ``recoveredVersions(slots:registered:limit:publicKey:)``
    /// can recover: `contact`, `fof` (empty scope), `group`, `member` (a group
    /// id). Not `link` / `reply` (one random scope per capability, never
    /// rotated) and not `channel` (its version is the channel's seed epoch,
    /// which this device reads from the channel itself).
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
        switch reachClass {
        case .contact, .fof, .group, .member: return true
        case .link, .reply, .channel: return false
        }
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

    /// The version of `(class, scope)` in `state`, or ``initialVersion``.
    public static func version(
        of reachClass: FfiReachClass, scope: Data, in state: FfiReachState
    ) -> UInt32 {
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
    /// sections 1 and 6).
    ///
    /// - `contact`: always — every contact holds it.
    /// - `member` per group the account is in, whatever the setting (a
    ///   co-member must always be able to deliver a Welcome for the owner's new
    ///   device).
    /// - `group` per group the setting lets find the owner
    ///   (`allowsScope`).
    /// - `link` per live link, `reply` per unexpired reply.
    /// - `fof` while contacts of contacts may find the owner.
    ///
    /// Expired links and replies are left out: the DS drops them anyway, and
    /// leaving them out makes the next sync unregister them.
    public static func desiredKeys(
        setting: FfiVisibilitySetting,
        state: FfiReachState,
        groupIds: [Data],
        links: [LinkReach],
        replies: [ReplyReach],
        channels: [(id: Data, epoch: UInt32)] = [],
        nowMs: UInt64,
        allowsScope: (FfiVisibilitySetting, FfiScopeId) -> Bool,
        allowsFof: (FfiVisibilitySetting) -> Bool
    ) -> [ReachKeySpec] {
        var out: [ReachKeySpec] = []
        out.append(ReachKeySpec(
            reachClass: .contact, scopeId: Data(),
            version: version(of: .contact, scope: Data(), in: state), expiresAtMs: nil))
        for groupId in groupIds where !groupId.isEmpty && groupId.count <= 64 {
            out.append(ReachKeySpec(
                reachClass: .member, scopeId: groupId,
                version: version(of: .member, scope: groupId, in: state), expiresAtMs: nil))
            if allowsScope(setting, groupScope(groupId)) {
                out.append(ReachKeySpec(
                    reachClass: .group, scopeId: groupId,
                    version: version(of: .group, scope: groupId, in: state), expiresAtMs: nil))
            }
        }
        for link in links where link.expiresAtMs > nowMs {
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
        // `channel` per channel the setting lets find the owner, versioned
        // by the channel's current seed epoch (ADR-0060 section 3).
        for channel in channels where !channel.id.isEmpty && channel.id.count <= 64 {
            if allowsScope(setting, FfiScopeId(kind: .channel, id: channel.id)) {
                out.append(ReachKeySpec(
                    reachClass: .channel, scopeId: channel.id, version: channel.epoch,
                    expiresAtMs: nil))
            }
        }
        if allowsFof(setting) {
            out.append(ReachKeySpec(
                reachClass: .fof, scopeId: Data(),
                version: version(of: .fof, scope: Data(), in: state), expiresAtMs: nil))
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

        public init(grant: FfiReachGrant, scopeHex: String?, storedAtMs: UInt64) {
            self.classRaw = ReachRules.className(grant.class)
            self.version = grant.version
            self.seedB64 = grant.seed.base64EncodedString()
            self.scopeHex = scopeHex
            self.storedAtMs = storedAtMs
        }

        public var reachClass: FfiReachClass? { ReachRules.reachClass(named: classRaw) }
        public var seed: Data? { Data(base64Encoded: seedB64) }
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
    /// then any `member`, `group`, `channel`, `reply` — newest first within a
    /// class. A Welcome writer tries them in this order and moves to the next
    /// on a `401` (ADR-0050 2.5a item 7): one key the owner rotated away must
    /// not cost a Welcome another key still opens.
    public static func writeSeeds(from held: [HeldGrant], groupIdHex: String?) -> [Data] {
        func all(_ cls: FfiReachClass, scope: String?? = nil) -> [Data] {
            held.filter { $0.reachClass == cls && (scope == nil || $0.scopeHex == scope!) }
                .sorted { $0.storedAtMs > $1.storedAtMs }
                .compactMap(\.seed)
        }
        var ordered: [Data] = all(.contact)
        if let groupIdHex { ordered += all(.member, scope: .some(groupIdHex)) }
        ordered += all(.member)
        ordered += all(.group)
        ordered += all(.channel)
        ordered += all(.reply)
        var seen = Set<Data>()
        return ordered.filter { $0.count == 32 && seen.insert($0).inserted }
    }

    /// Replace the grants held for one person with `incoming` of the same
    /// class and scope — the newest wins — keeping every other.
    public static func merging(_ incoming: HeldGrant, into held: [HeldGrant]) -> [HeldGrant] {
        // A replayed older version never displaces a newer one of the same
        // class and scope (a re-sent profile carries whatever it carried).
        if held.contains(where: {
            $0.classRaw == incoming.classRaw && $0.scopeHex == incoming.scopeHex
                && $0.version > incoming.version
        }) {
            return held
        }
        var out = held.filter {
            !($0.classRaw == incoming.classRaw && $0.scopeHex == incoming.scopeHex)
        }
        out.append(incoming)
        return out
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
