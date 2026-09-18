// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which `UUID` a call is reported to the system under, when the two routes
/// that can report it learn the call's identity at different times.
///
/// ## The problem this exists for
///
/// CallKit identifies a call by a `UUID` chosen at report time, and reporting
/// the same call twice under two UUIDs rings the phone twice. Lakko has exactly
/// that hazard, because the two routes know different things:
///
///   * The **VoIP push** must report a call before its handler returns (iOS
///     terminates an app that does not), and the wake payload deliberately
///     carries no call id — it carries an opaque routing id and a journal
///     pointer, and nothing else (invariants #2 and #5). So it reports under a
///     freshly minted UUID.
///   * The **signaling row**, fetched and decrypted moments later, carries the
///     real 16-byte call id, whose natural UUID (``IncomingCallReport/uuid(forCallId:)``)
///     is a different value.
///
/// So the placeholder has to be ADOPTED by the call id that arrives after it,
/// rather than a second call being reported.
///
/// ## Why one pending placeholder is enough
///
/// Lakko is a one-call-at-a-time app (``CallSlot``), and the provider tells the
/// system so (`maximumCallsPerCallGroup = 1`). So at most one VoIP-reported
/// call can be waiting for its identity at any moment, and the adoption needs
/// no matching on group, pointer or time — the pending placeholder IS the call
/// that is ringing. Matching on the wake's routing id was the alternative and
/// it buys nothing: it would have to reverse-map a `push_gid` that a stray or
/// replayed wake may not resolve, and be wrong in exactly the case the
/// placeholder timeout already covers.
@MainActor
final class CallKitCallRegistry {

    /// The process-wide registry. Shared for the same reason ``CallKitProvider``
    /// is: the VoIP push handler (owned by the app delegate) and the call bridge
    /// (built next to ``CallHost``) are two unrelated trees that must agree on
    /// one call's identity. Every consumer takes it as an injected parameter, so
    /// a test drives its own.
    static let shared = CallKitCallRegistry()

    /// A call reported from a VoIP push whose call id is not known yet.
    private(set) var pendingPlaceholder: UUID?
    /// Call id (lowercase hex) -> the UUID the system knows that call by.
    private var adopted: [String: UUID] = [:]

    init() {}

    /// Record that a placeholder call has been reported to the system.
    ///
    /// A placeholder that was already pending is REPLACED and returned, so the
    /// caller can end the older one: two VoIP pushes with no call arriving in
    /// between mean the first call is gone, and leaving it ringing forever is
    /// both wrong and the thing iOS penalises an app for.
    @discardableResult
    func placeholderReported(_ uuid: UUID) -> UUID? {
        let superseded = pendingPlaceholder
        pendingPlaceholder = uuid
        return superseded == uuid ? nil : superseded
    }

    /// The UUID this call is (or will be) known by: the one already adopted for
    /// it, else the pending placeholder — adopted here — else the call id's own
    /// UUID.
    ///
    /// `nil` only for a malformed call id (not 16 bytes), which no real call
    /// has: the id is minted as 16 random bytes and carried as a fixed-width
    /// field on the wire.
    func uuid(forCallId callId: Data) -> UUID? {
        let hex = IncomingCallReport.hex(callId)
        if let known = adopted[hex] { return known }
        if let placeholder = pendingPlaceholder {
            pendingPlaceholder = nil
            adopted[hex] = placeholder
            return placeholder
        }
        guard let derived = IncomingCallReport.uuid(forCallId: callId) else { return nil }
        adopted[hex] = derived
        return derived
    }

    /// Forget a finished call, by its id.
    func release(callId: Data) {
        adopted.removeValue(forKey: IncomingCallReport.hex(callId))
    }

    /// Forget a finished call, by the UUID the system named.
    func release(uuid: UUID) {
        if pendingPlaceholder == uuid { pendingPlaceholder = nil }
        adopted = adopted.filter { $0.value != uuid }
    }

    /// Drop the pending placeholder — the wake never produced a call.
    func releasePlaceholder() {
        pendingPlaceholder = nil
    }

    /// Logout / account switch.
    func reset() {
        pendingPlaceholder = nil
        adopted.removeAll()
    }
}
