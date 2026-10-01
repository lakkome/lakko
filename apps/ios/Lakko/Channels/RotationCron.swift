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
        /// Publisher's MLS group for this channel.
        public let publisherGroup: PublisherGroup
        /// Publisher's MLS identity + signing key.
        public let identity: Identity
        /// Hex-encoded 16-byte channel id (URL slot).
        public let channelIdHex: String
        /// HTTP client for the `/v1/channels/...` surface.
        public let channelsClient: ChannelsClient
        /// Caller-persistence: invoked synchronously with
        /// the new state after a successful fanout.
        public let saveState: @Sendable (ChannelRotationState) async throws -> Void
        /// Optional callback after a successful rotation
        /// (carries the new server-side epoch).
        public let onRotated: (@Sendable (UInt32) -> Void)?

        public init(
            publisherGroup: PublisherGroup,
            identity: Identity,
            channelIdHex: String,
            channelsClient: ChannelsClient,
            saveState: @escaping @Sendable (ChannelRotationState) async throws -> Void,
            onRotated: (@Sendable (UInt32) -> Void)? = nil
        ) {
            self.publisherGroup = publisherGroup
            self.identity = identity
            self.channelIdHex = channelIdHex
            self.channelsClient = channelsClient
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

        // 1. Advance the local MLS epoch + derive the new
        //    seed BEFORE talking to the server. If the
        //    server-side POST /rotate fails later, we have
        //    already rotated the local PublisherGroup —
        //    that's fine because the next tick retries
        //    from the new local state, and the server's
        //    atomic-rotation guarantee means no partial
        //    landings are observable.
        let signerBefore = try? context.publisherGroup.dsAccessSigner(identity: context.identity)
        _ = try context.publisherGroup.rotate(identity: context.identity)
        // ADR-0053 — a rotation moves the PublisherGroup epoch, so the journal's
        // access key moves with it: register the new key proven under the old
        // one, or this device's own later journal reads would be refused. The
        // rotation commit is not published to the journal, so no peer can hold
        // an unregistered epoch; best effort, and the co-publisher panel's
        // claim-on-open is not a repair for a missed one.
        if let signerBefore,
           let signerAfter = try? context.publisherGroup.dsAccessSigner(identity: context.identity),
           let groupId = try? context.publisherGroup.groupId() {
            do {
                try await DsClient().claimGroup(
                    groupId: groupId,
                    bodyJson: signerAfter.groupClaimBodyJson(),
                    provenBy: signerBefore)
            } catch {
                NSLog("ADR-0053: PublisherGroup rotation epoch not registered: %@",
                      String(describing: error))
            }
        }
        let newSeed = try context.publisherGroup.channelSeed(identity: context.identity)
        let channelIdBytes = try context.publisherGroup.channelId()

        // 2. The new server epoch the envelopes will bind
        //    to. fetchEpoch returns the CURRENT server
        //    epoch; the rotate endpoint will bump to
        //    current+1.
        let currentEpoch = try await context.channelsClient.fetchEpoch(
            channelIdHex: context.channelIdHex
        )
        let newServerEpoch = currentEpoch &+ 1
        if newServerEpoch < currentEpoch {
            // Wrap-around (currentEpoch == UInt32.max). Refuse
            // to rotate further — operator should investigate.
            throw CronError.unexpectedServerEpoch(currentEpoch)
        }

        // 3. Walk the subscriber list + seal new envelopes.
        let subscribers = try await context.channelsClient.listSubscribers(
            channelIdHex: context.channelIdHex,
            accountIdentity: context.identity
        )
        var batch: [RotateBatchEntry] = []
        batch.reserveCapacity(subscribers.count)
        for subscriberHex in subscribers {
            guard let subscriberBytes = Self.hexToBytes(subscriberHex) else {
                throw CronError.malformedSubscriberHex(subscriberHex)
            }
            let envelope = try sealSubscriberSeedEnvelope(
                subscriberPub: subscriberBytes,
                channelId: channelIdBytes,
                epochId: newServerEpoch,
                seed: newSeed
            )
            batch.append(
                RotateBatchEntry(
                    subscriberPubkeyHex: subscriberHex,
                    envelopeB64: envelope.base64EncodedString()
                )
            )
        }

        // 4. Fan the sealed batch out via the ADR-0030
        //    chunked staged-rotation protocol. Stage the
        //    envelopes in slices of at most
        //    `maxStageChunkEntries` (the server rejects a
        //    larger chunk with 413), each an idempotent
        //    upsert into the staging area — the live epoch is
        //    NOT bumped and staged rows are never served, so a
        //    chunk that throws just means the next tick
        //    re-stages from the same local state. Only after
        //    every chunk is staged does the single atomic
        //    commit promote them + bump the epoch (C-42: the
        //    commit is the one atomic point). An empty batch
        //    stages nothing and still commits, bumping the
        //    epoch on schedule for a channel with no
        //    subscribers yet.
        let chunkSize = ChannelsClient.maxStageChunkEntries
        var offset = 0
        while offset < batch.count {
            let end = min(offset + chunkSize, batch.count)
            try await context.channelsClient.stageRotation(
                channelIdHex: context.channelIdHex,
                targetEpoch: newServerEpoch,
                batch: Array(batch[offset..<end]),
                accountIdentity: context.identity
            )
            offset = end
        }
        let committedEpoch = try await context.channelsClient.commitRotation(
            channelIdHex: context.channelIdHex,
            targetEpoch: newServerEpoch,
            accountIdentity: context.identity
        )

        // 5. Stamp the new state + persist.
        let newState = channelRotationStateAfterRotation(nowMs: now)
        try await context.saveState(newState)
        context.onRotated?(committedEpoch)

        return OnceResult(rotated: true, newState: newState, newEpoch: committedEpoch)
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

    /// Parse a lowercase / mixed-case hex string into
    /// bytes. Returns `nil` on any malformed input.
    private static func hexToBytes(_ hex: String) -> Data? {
        guard !hex.isEmpty, hex.count % 2 == 0 else { return nil }
        var out = Data(capacity: hex.count / 2)
        var iter = hex.unicodeScalars.makeIterator()
        while let hi = iter.next(), let lo = iter.next() {
            guard let hn = Self.hexNibble(hi.value),
                  let ln = Self.hexNibble(lo.value)
            else { return nil }
            out.append((hn << 4) | ln)
        }
        return out
    }

    private static func hexNibble(_ code: UInt32) -> UInt8? {
        switch code {
        case 0x30...0x39: return UInt8(code - 0x30)
        case 0x61...0x66: return UInt8(code - 0x61 + 10)
        case 0x41...0x46: return UInt8(code - 0x41 + 10)
        default: return nil
        }
    }
}
