// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The account's blocked people as this process sees them (ADR-0099
/// section 6, UX `reporting-and-moderation.md` section 1).
///
/// The register itself lives in the synced state that
/// ``DeviceGroupSyncRepository`` folds and carries between the account's own
/// devices; this is its projection, rebuilt from every fold
/// (``adopt(_:)``) so every receive path and every screen reads one answer
/// without reaching the repository. A write goes back through ``setter``,
/// which the composition root points at the synced-state write.
///
/// What a block DOES on each path is never decided here: every caller asks
/// `blockVerdict(path:blocked:)` in `crates/lakko-ui-rules` with the answer
/// this gives.
@MainActor
public final class BlockList: ObservableObject {
    public static let shared = BlockList()

    /// One blocked person, most recently blocked first.
    public struct Entry: Equatable, Identifiable {
        public let accountKey: Data
        public let blockedAtMs: UInt64
        public var id: String { base64Encode(accountKey) }
    }

    /// Most recently blocked first, as the register orders them.
    @Published public private(set) var entries: [Entry] = []
    private var keys: Set<Data> = []

    /// The synced-state write: `(accountKey, blocked) -> written`. `nil`
    /// until the composition root wires it; a write with no setter does
    /// nothing.
    public var setter: ((Data, Bool) -> Bool)?

    init() {}

    /// The synced state learned its set (from this or another device).
    public func adopt(_ accounts: [BlockedAccount]) {
        let next = accounts.map { Entry(accountKey: $0.accountKey, blockedAtMs: $0.blockedAtMs) }
        guard next != entries else { return }
        entries = next
        keys = Set(next.map(\.accountKey))
    }

    /// Whether the account behind `accountKey` (32 bytes) is blocked.
    public func isBlocked(_ accountKey: Data) -> Bool {
        keys.contains(accountKey)
    }

    /// The same for a base64 person key, as the receive paths carry it
    /// (``MemberIdentity``). `false` for `nil` or anything that does not
    /// decode: an unresolved sender is never treated as blocked.
    public func isBlocked(b64 key: MemberIdentity?) -> Bool {
        guard let key, let bytes = base64Decode(key) else { return false }
        return keys.contains(bytes)
    }

    /// Block or unblock. Returns whether the register holds the requested
    /// state afterwards on this device (the other devices learn it at their
    /// next sync).
    @discardableResult
    public func set(_ accountKey: Data, blocked: Bool) -> Bool {
        guard accountKey.count == 32, let setter else { return false }
        return setter(accountKey, blocked)
    }

    /// What blocking does beyond the register (UX 1.2: the person is taken
    /// off the contact list as `Remove contact` does). The composition root
    /// points it at ``ContactsRepository/block(accountKey:)``, which writes
    /// the register through ``set(_:blocked:)`` itself.
    public var blockAction: ((Data) -> Bool)?

    /// Block `accountKey`: the full action when wired, else the register
    /// alone.
    @discardableResult
    public func block(_ accountKey: Data) -> Bool {
        if let blockAction { return blockAction(accountKey) }
        return set(accountKey, blocked: true)
    }

    /// Unblock `accountKey` (UX 1.3): it does not make them a contact again.
    @discardableResult
    public func unblock(_ accountKey: Data) -> Bool {
        set(accountKey, blocked: false)
    }

    /// The verdict for one path, for the person behind `key`.
    public func verdict(_ path: BlockPath, b64 key: MemberIdentity?) -> BlockVerdict {
        blockVerdict(path: path, blocked: isBlocked(b64: key))
    }

    /// The verdict for one path, for `accountKey`.
    public func verdict(_ path: BlockPath, accountKey: Data) -> BlockVerdict {
        blockVerdict(path: path, blocked: isBlocked(accountKey))
    }
}
