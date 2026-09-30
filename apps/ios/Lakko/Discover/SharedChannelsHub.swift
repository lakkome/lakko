// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0059 section 6 — channels people in your groups follow. The session
/// runtime; mirror of web `discover/SharedChannelsContext.tsx`.
///
/// Owns, for the signed-in account:
///
///   * the RECEIVE side: a member's list is filed under the member's ACCOUNT
///     key resolved at receipt (C-68), strictly newer stamp wins
///     (`sharedChannelsIsNewer`), under a per-group serialising chain (C-62
///     sub-invariant 5: the comparison is only meaningful against CURRENT
///     stored state);
///   * the "Share my channels with this group" switch per group, and the SEND
///     side: the list is sent when the switch goes on, an empty list when it
///     goes off, again whenever the set of listed channels this device follows
///     changes (``resendChanged()``, which the session calls after a channel
///     change, debounced), and re-sent VERBATIM with its original stamp when
///     the group grows (throttled + jittered like the pins), so a newcomer
///     learns it and no member takes the re-send for a newer list;
///   * the sealed row both live in (``DiscoverStore``, `shared`).
///
/// Only channels whose own metadata says `listed` are shared
/// (``SharedChannels/toShare(_:)``, fed by ``shareable``), so a channel
/// reached through a private handle never is.
@MainActor
public final class SharedChannelsHub: ObservableObject {

    struct Ports {
        /// The signed-in account, or `nil` between sessions.
        var accountIdB64: @MainActor () -> String?
        /// Seal and publish `bytes` as an MLS application message in the group.
        var send: @MainActor (_ groupIdHex: String, _ bytes: Data) async throws -> Void
        /// Whether this device can still send into the group.
        var canSend: @MainActor (_ groupIdHex: String) -> Bool
        var nowMs: @MainActor () -> Int64
    }

    /// The row: received lists, what was sent, and the switches. Published so
    /// the group-info switch, the Settings card and Discover redraw.
    @Published public private(set) var row = SharedChannels.Row()

    /// The listed channels this device follows, as the share rule reads them.
    /// Set by the signed-in shell, which owns the channel list.
    var shareable: @MainActor () -> [SharedChannels.Shareable] = { [] }

    private let ports: Ports
    private let store: DiscoverStore
    private let lock = GroupMlsLock()
    private var loadedAccount: String?
    private lazy var growth = PinPublisher(
        ports: PinPublisher.Ports(
            send: { [weak self] groupIdHex, bytes in
                try await self?.ports.send(groupIdHex, bytes)
            },
            envelopes: { [weak self] groupIdHex in
                self?.growthEnvelope(groupIdHex: groupIdHex).map { [$0] } ?? []
            },
            nowMs: { [weak self] in self?.ports.nowMs() ?? 0 },
            jitterMs: { Int64.random(in: 0...ProfilePublisher.maxJitterMs) },
            sleep: { ms in
                try? await Task.sleep(nanoseconds: UInt64(max(0, ms)) * 1_000_000)
            }
        )
    )

    init(ports: Ports, store: DiscoverStore = DiscoverStore()) {
        self.ports = ports
        self.store = store
    }

    /// Load the row for the signed-in account when it changed. Cheap when it
    /// did not; called at every entry point and by the screens on appear.
    public func bind() {
        let account = ports.accountIdB64()
        guard account != loadedAccount else { return }
        loadedAccount = account
        growth.reset()
        row = account.map { store.loadShared(accountIdB64: $0) } ?? SharedChannels.Row()
    }

    /// Whether the switch is on in the group.
    public func isSharing(groupIdHex: String) -> Bool {
        bind()
        return row.shareOn.contains(groupIdHex)
    }

    /// The channels this device would share now.
    func currentShareable() -> [SharedChannels.Channel] {
        SharedChannels.toShare(shareable())
    }

    private func persist() {
        guard let account = loadedAccount else { return }
        do {
            try store.saveShared(accountIdB64: account, row: row)
        } catch {
            NSLog("[shared-channels] saving failed")
        }
    }

    private static func compose(_ list: SharedChannels.List) throws -> Data {
        try composeSharedChannelsEnvelope(list: FfiSharedChannels(
            updatedAtMs: list.updatedAtMs,
            channels: try list.channels.map {
                FfiSharedChannel(
                    channelId: try ChannelsComposer.channelIdFromHex($0.channelIdHex),
                    handle: $0.handle,
                    name: $0.name)
            }))
    }

    /// Send `channels` fresh-stamped into the group and record it as sent.
    /// Runs under the group's chain.
    private func sendFresh(groupIdHex: String, channels: [SharedChannels.Channel]) async throws {
        let list = SharedChannels.List(
            updatedAtMs: SharedChannels.freshStamp(nowMs: ports.nowMs(), previous: row.sent[groupIdHex]),
            channels: channels)
        try await ports.send(groupIdHex, try Self.compose(list))
        row.sent[groupIdHex] = list
        persist()
        growth.notePublished(groupIdHex: groupIdHex)
    }

    /// Turn the switch on or off: sends the list (or an empty one), then
    /// stores the switch. Throws the send failure; the switch does not change.
    public func setSharing(groupIdHex: String, on: Bool) async throws {
        bind()
        guard loadedAccount != nil else { throw SharedChannelsError.signedOut }
        guard ports.canSend(groupIdHex) else { throw SharedChannelsError.notInGroup }
        try await lock.run(groupIdHex: groupIdHex) { [self] in
            try await sendFresh(groupIdHex: groupIdHex, channels: on ? currentShareable() : [])
            if on { row.shareOn.insert(groupIdHex) } else { row.shareOn.remove(groupIdHex) }
            persist()
        }
    }

    /// File an incoming list. Fire-and-forget; the whole read-decide-write
    /// runs under the group's chain.
    public func receive(groupIdHex: String, list: FfiSharedChannels, senderKey: MemberIdentity?) {
        bind()
        guard loadedAccount != nil else { return }
        let incoming = SharedChannels.List(
            updatedAtMs: list.updatedAtMs,
            channels: list.channels.map {
                SharedChannels.Channel(
                    channelIdHex: $0.channelId.map { String(format: "%02x", $0) }.joined(),
                    handle: $0.handle,
                    name: $0.name)
            })
        Task { @MainActor [weak self] in
            guard let self else { return }
            try? await self.lock.run(groupIdHex: groupIdHex) { [weak self] in
                guard let self else { return }
                let (next, verdict) = SharedChannels.applyIncoming(
                    row: self.row, groupIdHex: groupIdHex, memberKeyB64: senderKey,
                    incoming: incoming,
                    isNewer: { sharedChannelsIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) })
                guard verdict == .accept else { return }
                self.row = next
                self.persist()
            }
        }
    }

    /// The group gained a member: schedule the verbatim re-send.
    public func noteGrowth(groupIdHex: String) {
        bind()
        guard row.shareOn.contains(groupIdHex) else { return }
        growth.noteGrowth(groupIdHex: groupIdHex)
    }

    /// The re-send the growth path carries: what was last sent, with its
    /// ORIGINAL stamp. `nil` when the switch is off or the group is gone.
    private func growthEnvelope(groupIdHex: String) -> Data? {
        guard row.shareOn.contains(groupIdHex), ports.canSend(groupIdHex),
              let sent = row.sent[groupIdHex]
        else { return nil }
        return try? Self.compose(sent)
    }

    /// A change of the followed listed set: re-send to every group the switch
    /// is on in whose last list differs. Compared against what was sent, so a
    /// relaunch that changes nothing sends nothing.
    public func resendChanged() async {
        bind()
        guard loadedAccount != nil else { return }
        let channels = currentShareable()
        for groupIdHex in row.shareOn.sorted() {
            if let sent = row.sent[groupIdHex], sent.channels == channels { continue }
            guard ports.canSend(groupIdHex) else { continue }
            do {
                try await lock.run(groupIdHex: groupIdHex) { [self] in
                    try await sendFresh(groupIdHex: groupIdHex, channels: channels)
                }
            } catch {
                NSLog("[shared-channels] re-send failed")
            }
        }
    }

    /// The chat was deleted from this device: forget everything about it.
    public func forgetGroup(groupIdHex: String) {
        bind()
        row = SharedChannels.forgetGroup(row: row, groupIdHex: groupIdHex)
        persist()
    }

    /// Session ended (logout / account switch): drop the in-memory row and
    /// cancel the scheduled re-sends. The file stays until `forgetDevice`.
    public func reset() {
        growth.reset()
        loadedAccount = nil
        row = SharedChannels.Row()
    }
}

public enum SharedChannelsError: Error, Equatable, LocalizedError {
    case signedOut
    case notInGroup

    public var errorDescription: String? {
        switch self {
        case .signedOut: return "Sign in again."
        case .notInGroup: return "You are no longer in this group."
        }
    }
}
