// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0048 §2, trigger 4 — when this device asks a group to introduce
/// itself, and what it remembers about having asked.
///
/// ## Why a pull exists at all
///
/// A profile reaches a group by push only: on join, on change, and when a
/// device notices the group grew (`ProfilePublisher.noteGrowth`). That last
/// trigger fires on the OTHER devices, and a device that is away when the
/// join lands never re-sends — so the newcomer sits with `Member` for every
/// such person and has nothing it can do about it (seen live on 2026-09-19,
/// three of five unnamed on the device that joined last). The request is the
/// newcomer's pull: one field-less envelope, answered by every member through
/// the same throttled, jittered growth path.
///
/// ## The rule
///
/// The decision itself is the core's `shouldRequestProfiles` — a join always
/// asks; otherwise ask only while some member is still unnamed here, and no
/// sooner than `profileRequestMinIntervalMs` after the last time. It lives in
/// Rust so the three clients cannot drift on it; this type only gathers the
/// inputs and records the outcome. The rule arrives through ``Ports/rule`` so
/// the unit tests can drive the orchestration without a live group, and one
/// test pins the real FFI rule against the same inputs.
///
/// ## What is recorded, and when
///
/// The stamp is written AFTER a successful send, never before: a request that
/// did not reach the journal has asked nobody, and recording it would silence
/// this device for a day over a network blip. The stamp is per `(account,
/// group)` in `UserDefaults` — a non-secret timestamp about a group id this
/// device already knows, the same shape as ``KnownGroupsStore``.
@MainActor
final class ProfileRequester {

    struct Ports {
        /// Seal and publish the request as an MLS application message.
        var send: @MainActor (_ groupIdHex: String) async throws -> Void
        /// How many members of the group, the viewer excluded, still resolve
        /// to the neutral placeholder on this device; `nil` when the roster
        /// cannot be read, which asks nothing (there is no count to ask on
        /// behalf of, and a torn read is not evidence that anyone is unnamed).
        var unnamedMembers: @MainActor (_ groupIdHex: String) -> Int?
        /// Whether this device can still send into the group (not evicted).
        var canSend: @MainActor (_ groupIdHex: String) -> Bool
        /// When this device last asked this group, `nil` if never.
        var lastRequestedAtMs: @MainActor (_ groupIdHex: String) -> UInt64?
        /// Remember that this device asked this group at `atMs`.
        var recordRequestedAt: @MainActor (_ groupIdHex: String, _ atMs: UInt64) -> Void
        var nowMs: @MainActor () -> UInt64
        /// The core's `shouldRequestProfiles(joinedJustNow:unnamedMembers:
        /// lastRequestedAtMs:nowMs:)`, injected so a test needs no FFI.
        var rule: @MainActor (
            _ joinedJustNow: Bool, _ unnamedMembers: UInt32, _ lastRequestedAtMs: UInt64?,
            _ nowMs: UInt64
        ) -> Bool
    }

    /// What one trigger did — returned so a caller (and a test) can tell a
    /// request that went out from one the rule declined and one that failed.
    enum Outcome: Equatable {
        /// The request was published and the stamp recorded.
        case requested
        /// The rule said no — nothing unnamed, or asked too recently.
        case notDue
        /// This device cannot send into the group, or could not read its
        /// roster; nothing was asked and nothing recorded.
        case cannotAsk
        /// The publish threw; nothing recorded, so the next trigger asks again.
        case sendFailed
    }

    private let ports: Ports

    init(ports: Ports) {
        self.ports = ports
    }

    /// This device just joined `groupIdHex`: ask now, whatever it asked
    /// before. A new membership is a new audience, and the join is the one
    /// moment every existing profile is guaranteed to be unreadable here (each
    /// was published in an epoch this device cannot open).
    @discardableResult
    func noteJoined(groupIdHex: String) async -> Outcome {
        await request(groupIdHex: groupIdHex, joinedJustNow: true)
    }

    /// The chat for `groupIdHex` was opened: ask if some member is still
    /// unnamed here and this device has not asked within the interval. This
    /// is what heals a group joined before the request existed, and what
    /// keeps the request from repeating while the answers are simply not
    /// back yet.
    @discardableResult
    func noteOpened(groupIdHex: String) async -> Outcome {
        await request(groupIdHex: groupIdHex, joinedJustNow: false)
    }

    /// ADR-0060 section 4, "A device that holds nothing asks": this device
    /// holds no offer from the other person of this one-to-one.
    /// The answer also sends them again. Nobody needs to be unnamed. With no
    /// `contact` grant it asks by the rule's interval; with one, every
    /// ``ReachRules/introductionsAskWithGrantMinMs``. Both read and write the
    /// stamp every request shares.
    @discardableResult
    func noteIntroductionsMissing(groupIdHex: String, holdsContactGrant: Bool) async -> Outcome {
        guard ports.canSend(groupIdHex) else { return .cannotAsk }
        guard holdsContactGrant else {
            return await ask(groupIdHex: groupIdHex, joinedJustNow: false, unnamed: 1)
        }
        let now = ports.nowMs()
        guard ReachRules.asksWithContactGrant(
            lastRequestedAtMs: ports.lastRequestedAtMs(groupIdHex), nowMs: now
        ) else { return .notDue }
        return await publish(groupIdHex: groupIdHex, now: now)
    }

    private func request(groupIdHex: String, joinedJustNow: Bool) async -> Outcome {
        guard ports.canSend(groupIdHex) else { return .cannotAsk }
        // A join asks regardless of the count, so a torn roster read must not
        // stop it; only the on-open trigger has nothing to say without one.
        let unnamed: Int
        if let count = ports.unnamedMembers(groupIdHex) {
            unnamed = count
        } else if joinedJustNow {
            unnamed = 0
        } else {
            return .cannotAsk
        }
        return await ask(groupIdHex: groupIdHex, joinedJustNow: joinedJustNow, unnamed: unnamed)
    }

    private func ask(groupIdHex: String, joinedJustNow: Bool, unnamed: Int) async -> Outcome {
        let now = ports.nowMs()
        guard ports.rule(
            joinedJustNow,
            UInt32(clamping: max(0, unnamed)),
            ports.lastRequestedAtMs(groupIdHex),
            now
        ) else { return .notDue }
        return await publish(groupIdHex: groupIdHex, now: now)
    }

    private func publish(groupIdHex: String, now: UInt64) async -> Outcome {
        do {
            try await ports.send(groupIdHex)
        } catch {
            if !ConversationReentryRules.isBehindRefusal(error) { NSLog("ADR-0048 profile request not sent: \(error)") }
            return .sendFailed
        }
        // Stamped after the send so a failed publish keeps the next trigger
        // live; stamped with the time the decision was taken so two triggers
        // in one session compare against the same clock the rule saw.
        ports.recordRequestedAt(groupIdHex, now)
        return .requested
    }
}

/// Per-account map `groupIdHex -> last request time (ms since the epoch)` in
/// `UserDefaults`. Same reasoning as ``KnownGroupsStore``: a tiny
/// non-secret map namespaced by account so two accounts on one device do not
/// collide. App-private — the notification extension never asks.
public final class ProfileRequestStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func lastRequestedAtMs(accountIdB64: String, groupIdHex: String) -> UInt64? {
        guard let raw = map(accountIdB64: accountIdB64)[groupIdHex] else { return nil }
        // Stored as a decimal string: `UserDefaults` round-trips numbers
        // through `NSNumber`, and a `UInt64` above `Int64.max` would not
        // survive that. A string that does not parse reads as "never asked",
        // which errs toward asking once more rather than never again.
        return UInt64(raw)
    }

    public func record(accountIdB64: String, groupIdHex: String, atMs: UInt64) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var current = map(accountIdB64: accountIdB64)
        current[groupIdHex] = String(atMs)
        defaults.set(current, forKey: Self.key(accountIdB64))
    }

    /// Forget one group this device is no longer in (ADR-0087 section 9).
    public func forgetGroup(accountIdB64: String, groupIdHex: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var current = map(accountIdB64: accountIdB64)
        guard current.removeValue(forKey: groupIdHex) != nil else { return }
        defaults.set(current, forKey: Self.key(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    private func map(accountIdB64: String) -> [String: String] {
        defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: String] ?? [:]
    }

    static func key(_ accountIdB64: String) -> String {
        "profile_request.\(accountIdB64)"
    }
}
