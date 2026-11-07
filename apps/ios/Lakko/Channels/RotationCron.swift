// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// G4-2-rotation-timer-ios — iOS-side rotation orchestrator.
///
/// Swift mirror of `apps/web/src/channels/rotation-cron.ts`.
/// The Rust core
/// (`crates/lakko-channels/src/rotation_policy.rs`) decides
/// _when_ to rotate via the pure
/// `channelShouldRotate(policy, state, nowMs)` UniFFI free
/// function. The orchestrator here decides _how_: it walks
/// the subscriber list, reseals the new content seed for
/// each subscriber via `sealSubscriberSeedEnvelope`, then
/// fans the batch out via the ADR-0030 chunked
/// staged-rotation protocol: the sealed envelopes are
/// uploaded in bounded chunks to a staging area
/// (`POST /v1/channels/:id/rotate/stage`, epoch NOT bumped,
/// staged rows never served), then a single atomic
/// `POST /v1/channels/:id/rotate/commit` promotes them and
/// bumps the epoch (C-42 preserved — the commit is the one
/// atomic point, so a subscriber never observes a torn
/// rotation).
///
/// Two entry points:
///   - [`runOnce`] — single-pass; returns the new rotation
///     state. Use this from a "Rotate now" button in
///     SwiftUI.
///   - [`start`] — periodic poller. Calls `runOnce` on
///     every tick, persists state via the caller-supplied
///     `saveState`, surfaces errors via `onError`. Returns
///     a `Task` whose `cancel()` stops the timer.
///
/// The orchestrator is **idempotent under no-op** — if the
/// policy says "no rotation needed", it returns the input
/// state unchanged + does no network round-trip.
///
/// **Failure semantics:**
///   - If `listSubscribers` / a `stageRotation` chunk /
///     `commitRotation` throws, the local rotation state is
///     NOT updated (so the next tick retries). Staging is
///     idempotent, so re-running from the same local state
///     just re-uploads any missing chunks; nothing is served
///     to subscribers until commit, so a mid-fanout failure
///     leaves the live epoch untouched (fail-closed, C-42).
///   - G-50: the rotation's effects (the DS epoch
///     registration, the reseal + commit) are recorded
///     durably WITH the N+1 provider snapshot before any of
///     them runs (``PublisherCommitEntry``); a failed step or
///     a restart resumes them from that record
///     (``ChannelPublisherCommits``), never from a fresh fetch
///     of the server epoch.
///   - If a single envelope seal fails, the WHOLE batch is
///     aborted — same atomic posture the server enforces.
///     Partial fanouts would leave some subscribers on the
///     old seed + some on the new, which is worse than
///     retrying.
///   - If `commitRotation` succeeds but the local
///     `saveState` closure throws, the rotation is
///     committed server-side; the caller's storage is
///     stale and the next tick will trigger another
///     rotation. That's the deliberate trade-off: we
///     cannot un-commit the server, but we MUST persist
///     the new state to avoid an infinite rotation loop.
public actor RotationCron {

    /// Snapshot the orchestrator needs to drive one
    /// rotation pass. Bundled so the call site cannot
    /// forget a field.
    public struct Context {
        /// What the rotation's steps run against: the channel, the
        /// account's current seed (the channel provider is built from the
        /// lineage seed it implies; requests are signed by the current
        /// one), the clients.
        public let commits: ChannelPublisherCommits.Context
        /// Where the N+1 content seed and server epoch are written back.
        public weak var state: ChannelsUiState?
        /// Caller-persistence: invoked synchronously with
        /// the new state after a successful fanout.
        public let saveState: @Sendable (ChannelRotationState) async throws -> Void
        /// Optional callback after a successful rotation
        /// (carries the new server-side epoch).
        public let onRotated: (@Sendable (UInt32) -> Void)?

        /// Hex-encoded 16-byte channel id (URL slot).
        public var channelIdHex: String { commits.channelIdHex }

        public init(
            commits: ChannelPublisherCommits.Context,
            state: ChannelsUiState?,
            saveState: @escaping @Sendable (ChannelRotationState) async throws -> Void,
            onRotated: (@Sendable (UInt32) -> Void)? = nil
        ) {
            self.commits = commits
            self.state = state
            self.saveState = saveState
            self.onRotated = onRotated
        }
    }

    /// Result of one single-pass rotation attempt.
    public struct OnceResult {
        /// `true` if the policy fired AND the rotation
        /// completed end-to-end.
        public let rotated: Bool
        /// New rotation state to persist. For
        /// `rotated:false` cases this is byte-identical to
        /// the input state.
        public let newState: ChannelRotationState
        /// New server-side epoch id after the rotation,
        /// when `rotated:true`.
        public let newEpoch: UInt32?
    }

    /// Errors `runOnce` can throw before any network
    /// round-trip. Network errors propagate as the
    /// `ChannelsClient`'s own error types.
    public enum CronError: Error {
        /// One subscriber's hex pubkey did not parse.
        case malformedSubscriberHex(String)
        /// The server returned a negative / nonsensical
        /// current epoch (should never happen — included
        /// for completeness).
        case unexpectedServerEpoch(UInt32)
    }

    private let context: Context

    public init(context: Context) {
        self.context = context
    }

    /// Run a single rotation pass. Returns
    /// `{ rotated: false }` if the policy says no;
    /// otherwise drives the full chain.
    ///
    /// The caller MUST persist `result.newState` (the
    /// `start` driver does this automatically via
    /// `context.saveState`).
    public func runOnce(
        policy: ChannelRotationPolicy,
        state: ChannelRotationState,
        nowMs: UInt64? = nil
    ) async throws -> OnceResult {
        let now = nowMs ?? Self.currentMs()
        if !channelShouldRotate(policy: policy, state: state, nowMs: now) {
            return OnceResult(rotated: false, newState: state, newEpoch: nil)
        }

        // 1. The server epoch the envelopes will bind to,
        //    read BEFORE the local rotation: after the eager
        //    merge nothing may fail before the commit's record
        //    is durable (G-50), and this is a network read.
        let currentEpoch = try await context.commits.channelsClient.fetchEpoch(
            channelIdHex: context.channelIdHex
        )
        let newServerEpoch = currentEpoch &+ 1
        if newServerEpoch < currentEpoch {
            // Wrap-around (currentEpoch == UInt32.max). Refuse
            // to rotate further — operator should investigate.
            throw CronError.unexpectedServerEpoch(currentEpoch)
        }

        // 2. Advance the local MLS epoch (the self-update
        //    commit eager-merges N -> N+1 in the channel
        //    provider) and record the rotation, then the N+1
        //    provider snapshot, durably, before any of its
        //    steps runs. A refused record runs nothing.
        let entry = try await Self.recordRotation(
            context.commits, serverTargetEpoch: newServerEpoch)

        // 3. The steps: the N+1 epoch key and the rotation
        //    commit on the DS in one request (ADR-0057,
        //    ADR-0081; best effort for a rotation), reseal the
        //    N+1 seed to every subscriber and commit the
        //    rotation on the channels service with the ADR-0030
        //    chunked fanout (C-42: the commit is the one atomic
        //    point).
        try await Self.runEntry(entry, context: context)

        // 4. Stamp the new state + persist.
        let newState = channelRotationStateAfterRotation(nowMs: now)
        try await context.saveState(newState)
        context.onRotated?(newServerEpoch)

        return OnceResult(rotated: true, newState: newState, newEpoch: newServerEpoch)
    }

    @MainActor
    private static func recordRotation(
        _ commits: ChannelPublisherCommits.Context, serverTargetEpoch: UInt32
    ) throws -> PublisherCommitEntry {
        let (identity, group) = try commits.open()
        // ADR-0081 — the proof of the epoch the rotation leaves, taken before
        // the eager merge and recorded with the rotation, so a replay after a
        // restart can still prove it.
        let proofEpoch = try group.epoch()
        let proofSeed = try group.dsAccessSeed(identity: identity)
        let commit = try group.rotate(identity: identity)
        let entry = PublisherCommitEntry(
            kind: .rotation,
            channelIdHex: commits.channelIdHex.lowercased(),
            groupIdHex: DsClient.pseudonymHex(try group.groupId()),
            targetEpoch: try group.epoch(),
            serverTargetEpoch: serverTargetEpoch,
            commitB64: commit.base64EncodedString(),
            proofEpoch: proofEpoch,
            proofSeedB64: proofSeed.base64EncodedString())
        try ChannelPublisherCommits.record(entry, accountIdB64: commits.accountIdB64)
        return entry
    }

    @MainActor
    private static func runEntry(
        _ entry: PublisherCommitEntry, context: Context
    ) async throws {
        try await PublisherCommitRunner.run(
            entry, ports: ChannelPublisherCommits.ports(context.commits, state: context.state))
        ChannelPublisherCommits.refreshContentSeed(context.commits, state: context.state)
    }

    /// Start a periodic rotation poller. Returns a `Task`
    /// whose `.cancel()` stops the underlying loop. Safe
    /// to leave running for the lifetime of a channel
    /// screen — one `runOnce` per `pollInterval` is a
    /// cheap heartbeat (zero network calls when the
    /// policy says no).
    ///
    /// `pollInterval` defaults to 5 minutes — small enough
    /// that a 24-hour interval rotation lands within ~5 min
    /// of the scheduled moment, large enough that an idle
    /// app does not burn battery on policy checks.
    ///
    /// `loadState` is called on every tick so the cron
    /// picks up state changes written by other paths
    /// (e.g. the "Rotate now" button or a post-counter
    /// bump from `composePost`).
    public nonisolated func start(
        policy: ChannelRotationPolicy,
        pollInterval: TimeInterval = 5 * 60,
        loadState: @escaping @Sendable () async throws -> ChannelRotationState,
        onError: (@Sendable (Error) -> Void)? = nil
    ) -> Task<Void, Never> {
        Task { [weak self] in
            guard let self = self else { return }
            // Fire immediately so a tab that opens past
            // the rotation deadline catches up rather than
            // waiting one full pollInterval.
            await self.tick(policy: policy, loadState: loadState, onError: onError)
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(pollInterval * 1_000_000_000))
                if Task.isCancelled { break }
                await self.tick(policy: policy, loadState: loadState, onError: onError)
            }
        }
    }

    private func tick(
        policy: ChannelRotationPolicy,
        loadState: @Sendable () async throws -> ChannelRotationState,
        onError: (@Sendable (Error) -> Void)?
    ) async {
        do {
            let state = try await loadState()
            _ = try await runOnce(policy: policy, state: state)
        } catch {
            onError?(error)
        }
    }

    // MARK: - helpers

    private static func currentMs() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000.0)
    }
}

/// Per-channel persistence for the rotation state (`lastRotationMs` +
/// `postsSinceRotation`). Non-secret operational metadata — no key material
/// — so plain `UserDefaults` is appropriate (invariant #7 governs key
/// material only; the channel list itself is likewise not SQLCipher-backed
/// on iOS today).
///
/// Per account (2026-09-30): the state belongs to the account that publishes
/// the channel, so it is filed under that account
/// (`lakko.channels.rotation-state.v2.<account>.<channel>`), the way
/// ``AcceptedContactsStore`` files the contacts. It used to be keyed by the
/// channel alone. No account: nothing is read or written (``loadOrSeed`` then
/// hands back an unpersisted genesis state).
enum ChannelRotationStateStore {
    private static let prefix = "lakko.channels.rotation-state.v2."
    /// The device-wide prefix used before the state was per account. Never
    /// read; removed by ``clear(accountIdB64:defaults:)``.
    static let legacyPrefix = "lakko.channels.rotation-state.v1."

    private struct Persisted: Codable {
        let lastRotationMs: UInt64
        let postsSinceRotation: UInt32
    }

    private static func accountPrefix(_ accountIdB64: String) -> String {
        prefix + accountIdB64 + "."
    }

    static func load(
        accountIdB64: String?,
        channelId: String,
        defaults: UserDefaults = .standard
    ) -> ChannelRotationState? {
        guard
            let accountIdB64,
            let data = defaults.data(forKey: accountPrefix(accountIdB64) + channelId),
            let p = try? JSONDecoder().decode(Persisted.self, from: data)
        else { return nil }
        return ChannelRotationState(
            lastRotationMs: p.lastRotationMs,
            postsSinceRotation: p.postsSinceRotation
        )
    }

    static func save(
        accountIdB64: String?,
        channelId: String,
        state: ChannelRotationState,
        defaults: UserDefaults = .standard
    ) {
        guard let accountIdB64 else { return }
        // Not written once the account's session has ended
        // (``AccountStoreGate``): the rotation cron files its state from its
        // own actor after the rotation's network calls.
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        let p = Persisted(
            lastRotationMs: state.lastRotationMs,
            postsSinceRotation: state.postsSinceRotation
        )
        guard let data = try? JSONEncoder().encode(p) else { return }
        defaults.set(data, forKey: accountPrefix(accountIdB64) + channelId)
    }

    /// Forget-device: drop one account's rotation states (and the old
    /// device-wide rows). Other accounts' stay.
    static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        let mine = accountPrefix(accountIdB64)
        for key in defaults.dictionaryRepresentation().keys
        where key.hasPrefix(mine) || key.hasPrefix(legacyPrefix) {
            defaults.removeObject(forKey: key)
        }
    }

    /// Return the persisted state, or seed a genesis `after_rotation(now)`
    /// snapshot when absent — so the first scheduled check on an existing
    /// channel does not fire an immediate rotation (it lands ~one interval
    /// later instead).
    static func loadOrSeed(
        accountIdB64: String?,
        channelId: String,
        nowMs: UInt64,
        defaults: UserDefaults = .standard
    ) -> ChannelRotationState {
        if let existing = load(accountIdB64: accountIdB64, channelId: channelId, defaults: defaults) {
            return existing
        }
        let seeded = channelRotationStateAfterRotation(nowMs: nowMs)
        save(accountIdB64: accountIdB64, channelId: channelId, state: seeded, defaults: defaults)
        return seeded
    }
}
