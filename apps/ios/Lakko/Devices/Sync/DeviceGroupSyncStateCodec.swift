// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The cross-device sync state, written to this device's disk.
///
/// Drafts, pin / mute / archive, read cursors and the chat order travel
/// between a person's devices over the DeviceGroup journal, and until this
/// existed they travelled nowhere else: the four maps were `@Published`
/// properties and nothing more, so a relaunch started them empty.
///
/// The journal cannot refill them. This client polls from cursor 0 on every
/// launch, which looks like it would - but it restores the DeviceGroup MLS
/// snapshot first, so every row it re-fetches is one its own state has
/// already consumed and cannot decrypt a second time. It re-downloads the
/// whole journal on each launch in order to fail on all of it.
///
/// What is stored is a SNAPSHOT of the folded state, not the journal. That is
/// what makes restoring it safe: every entry carries the HLC of the payload
/// that produced it, the poll folds newer rows on top through the same
/// `shouldReplace` comparison it always used, and a sibling device's later
/// write therefore wins over a restored one exactly as it would over a live
/// one. Nothing here is authoritative; it is a head start.
///
/// Pure encode / decode, no I/O, so a test can run it. The bytes are sealed
/// by ``DeviceGroupSnapshotStore`` under the Keychain wrap key, because a
/// draft is a message the person has not sent yet.
///
/// The wire shape is the web client's byte for byte
/// (`devices/sync/sync-state-store.ts`). These blobs are never exchanged -
/// each device writes its own - but one description fitting both is worth
/// more than a shape chosen twice.
public enum DeviceGroupSyncStateCodec {

    /// Payload schema version this client writes.
    public static let version = 1

    /// The four maps, as one snapshot.
    public struct Snapshot: Equatable {
        public var drafts: [String: DraftState]
        public var flags: [String: FlagsState]
        public var readCursors: [String: ReadCursorState]
        public var order: OrderState?

        public init(
            drafts: [String: DraftState] = [:],
            flags: [String: FlagsState] = [:],
            readCursors: [String: ReadCursorState] = [:],
            order: OrderState? = nil
        ) {
            self.drafts = drafts
            self.flags = flags
            self.readCursors = readCursors
            self.order = order
        }

        /// Whether this holds anything worth writing.
        public var isEmpty: Bool {
            drafts.isEmpty && flags.isEmpty && readCursors.isEmpty && order == nil
        }
    }

    // MARK: - Wire shape
    //
    // `Data` and `UInt64` ride as base64 and as decimal strings: a UInt64 HLC
    // does not survive `JSONSerialization`'s number handling, and the HLC is
    // exactly what decides whose write wins.

    private struct WireDraft: Codable {
        let t: String
        let r: String
        let h: String
    }
    private struct WireFlags: Codable {
        let p: Bool
        let m: Bool
        let a: Bool
        let u: Bool
        let h: String
    }
    private struct WireCursor: Codable {
        let i: String
        let r: String
        let h: String
    }
    private struct WireOrder: Codable {
        let g: [String]
        let h: String
    }
    private struct WirePayload: Codable {
        let v: Int
        let d: [String: WireDraft]
        let f: [String: WireFlags]
        let c: [String: WireCursor]
        let o: WireOrder?
    }

    // MARK: - Public API

    /// Encode the snapshot. `nil` only if JSON encoding itself fails, which
    /// for this shape it cannot.
    public static func encode(_ snapshot: Snapshot) -> Data? {
        let payload = WirePayload(
            v: version,
            d: snapshot.drafts.mapValues {
                WireDraft(
                    t: $0.text,
                    r: $0.replyToId.base64EncodedString(),
                    h: String($0.updatedAtHlc))
            },
            f: snapshot.flags.mapValues {
                WireFlags(
                    p: $0.pinned,
                    m: $0.muted,
                    a: $0.archived,
                    u: $0.unreadOverride,
                    h: String($0.updatedAtHlc))
            },
            c: snapshot.readCursors.mapValues {
                WireCursor(
                    i: $0.upToMessageId.base64EncodedString(),
                    r: String($0.readAtMs),
                    h: String($0.updatedAtHlc))
            },
            o: snapshot.order.map {
                WireOrder(
                    g: $0.groupIds.map { $0.base64EncodedString() },
                    h: String($0.updatedAtHlc))
            })
        return try? JSONEncoder().encode(payload)
    }

    /// Decode a payload.
    ///
    /// A malformed ENTRY is dropped and the rest of the snapshot is kept:
    /// losing one chat's pin is better than losing every chat's, and the poll
    /// can refill any single entry from a sibling's next write. Bytes that
    /// are not this schema at all return `nil`, which the caller treats as
    /// "nothing stored" - the state a first run is in.
    public static func decode(_ data: Data) -> Snapshot? {
        guard let payload = try? JSONDecoder().decode(WirePayload.self, from: data),
              payload.v == version
        else {
            return nil
        }
        var out = Snapshot()
        for (hex, raw) in payload.d {
            guard let hlc = UInt64(raw.h),
                  let replyToId = Data(base64Encoded: raw.r)
            else { continue }
            out.drafts[hex] = DraftState(
                text: raw.t, replyToId: replyToId, updatedAtHlc: hlc)
        }
        for (hex, raw) in payload.f {
            guard let hlc = UInt64(raw.h) else { continue }
            out.flags[hex] = FlagsState(
                pinned: raw.p,
                muted: raw.m,
                archived: raw.a,
                unreadOverride: raw.u,
                updatedAtHlc: hlc)
        }
        for (hex, raw) in payload.c {
            guard let hlc = UInt64(raw.h),
                  let readAtMs = UInt64(raw.r),
                  let upTo = Data(base64Encoded: raw.i)
            else { continue }
            out.readCursors[hex] = ReadCursorState(
                upToMessageId: upTo, readAtMs: readAtMs, updatedAtHlc: hlc)
        }
        if let rawOrder = payload.o, let hlc = UInt64(rawOrder.h) {
            let ids = rawOrder.g.map { Data(base64Encoded: $0) }
            // The order is one value rather than a map, so a single bad id
            // would reorder the chat list silently. Either all of it decodes
            // or none does.
            if !ids.contains(where: { $0 == nil }) {
                out.order = OrderState(
                    groupIds: ids.compactMap { $0 }, updatedAtHlc: hlc)
            }
        }
        return out
    }
}
