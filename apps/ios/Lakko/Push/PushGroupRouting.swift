// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// ADR-0042 orchestration — mint / receive / derive / subscribe / notify
/// the opaque per-group push routing id (`push_gid`), closing the
/// invariant-#5 push social-graph leak.
///
/// Mirror of `apps/web/src/push/group-routing.ts`.
///
/// ## The load-bearing rule: OWNER mints, everyone else RECEIVES
///
/// `push_routing_secret` is 32 RANDOM bytes, so two members minting
/// independently would diverge and break routing (their `push_gid`s would
/// differ and the notify JOIN would never match). Therefore exactly ONE
/// source mints:
///
///   * Only the ADR-0022 group **OWNER** mints, once, and persists it
///     (``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``).
///     It then broadcasts the secret to the group over an E2EE MLS
///     application message (the DS sees ciphertext — invariant #2).
///   * Everyone else ONLY receives + persists the secret from that
///     envelope (``receiveSecret(accountIdB64:groupIdHex:secret:)``) — they
///     never mint.
///
/// ## Never subscribe under the leaky `group_id`
///
/// The whole point of the ADR is that push-gw + APNs/FCM must not see the
/// DS journal key. So a member that does NOT yet hold the secret
/// **defers** its push subscription (transient no-push until the secret
/// arrives) — it never falls back to subscribing under `group_id`
/// (``subscribeUnderPushGidBestEffort(accountIdB64:groupIdHex:)`` is a no-op
/// when no secret is held). The publisher always holds the secret (it is a
/// member), so notify always has a `push_gid`.
///
/// ## Concurrency
///
/// Provisioning can be triggered from two places in the same session — the
/// legacy-sweep on bootstrap and the chat view's onAppear. A per-group
/// in-flight lock (plus the synchronous mint→persist, which flips
/// ``PushRoutingStore/hasSecret(accountIdB64:groupIdHex:)`` true before the
/// first `await`) coalesces them so the random secret is minted exactly
/// once (a second concurrent mint would overwrite + double-broadcast a
/// DIFFERENT secret).
///
/// `@MainActor` so the owner-role check + the E2EE `broadcast` (which runs
/// `Group.createApplicationMessage` in ``ChatRepository``) stay on the same
/// actor that owns the MLS `Group` handle.
@MainActor
public final class PushGroupRouting {

    /// `push_routing_secret` length (bytes) — mirrors the Rust
    /// `PUSH_ROUTING_SECRET_LEN`.
    public static let secretLen: Int = 32

    /// Total distribution-envelope length (bytes): the 29-byte domain
    /// separator + 1 version byte + the 32-byte secret. Used only as a
    /// cheap receive-side pre-filter before the authoritative FFI decode.
    /// Mirrors `lakko_contacts::push_routing` (29 + 1 + 32).
    public static let envelopeLen: Int = 29 + 1 + secretLen

    /// First byte of the envelope domain separator (`'l'` of
    /// `"lakko/push-routing-secret\0"`). The cheap pre-filter; the FFI
    /// `decodePushRoutingSecret` is the authoritative fail-closed check.
    private static let envelopeFirstByte: UInt8 = 0x6c

    private let store: PushRoutingStore

    /// Subscribe seam — `(push_gid) -> ()`. Defaults to the real push-gw
    /// call (device pseudonym from the Keychain + ``PushClient``);
    /// injectable so tests can spy on the target without touching the
    /// network (mirror of the web `PushClient` `vi.mock`).
    private let subscribeUnderPushGid: (Data) async throws -> Void

    /// Per-group in-flight provisioning keys, so a concurrent legacy-sweep
    /// + onAppear for the same group mint the random secret exactly once.
    private var inflight: Set<String> = []

    /// `nonisolated` so the initializer is callable as a default argument
    /// from a nonisolated context (``ChatRepository/init``); it only assigns
    /// stored properties.
    public nonisolated init(
        store: PushRoutingStore = PushRoutingStore(),
        subscribeUnderPushGid: @escaping (Data) async throws -> Void = { pushGid in
            let devicePseudonym = try DevicePushId.getOrCreate()
            _ = try await PushClient().subscribeGroup(
                devicePseudonym: devicePseudonym,
                groupPseudonym: pushGid
            )
        }
    ) {
        self.store = store
        self.subscribeUnderPushGid = subscribeUnderPushGid
    }

    // MARK: - Primitives (mirror of group-routing.ts helpers)

    /// Mint a fresh `push_routing_secret` (32 CSPRNG bytes). OWNER ONLY —
    /// everyone else receives it over the distribution envelope.
    public nonisolated static func mintSecret() -> Data? {
        var bytes = Data(count: secretLen)
        let status = bytes.withUnsafeMutableBytes { ptr -> Int32 in
            guard let base = ptr.baseAddress else { return errSecAllocate }
            return SecRandomCopyBytes(kSecRandomDefault, secretLen, base)
        }
        return status == errSecSuccess ? bytes : nil
    }

    /// Derive the stable opaque `push_gid` (32 bytes) from a secret via the
    /// audited FFI keyed-BLAKE3 derivation. Deterministic: every member
    /// that holds the same secret derives the same `push_gid`.
    public nonisolated static func derivePushGid(secret: Data) -> Data? {
        try? LakkoCore.derivePushGid(secret: secret)
    }

    /// Compose the E2EE distribution-envelope bytes for a secret (FFI).
    public nonisolated static func composeSecretEnvelope(secret: Data) throws -> Data {
        try composePushRoutingSecret(secret: secret)
    }

    /// Attempt to decode an MLS application-message plaintext as a
    /// push-routing-secret distribution envelope. Returns the 32-byte
    /// secret, or `nil` when the bytes are not this envelope (fail-closed
    /// — the authoritative check is the FFI codec's domain + version +
    /// exact-length gate). A cheap length/first-byte pre-filter avoids an
    /// FFI call on every chat message.
    public nonisolated static func tryDecodeSecretEnvelope(_ bytes: Data) -> Data? {
        guard bytes.count == envelopeLen, bytes.first == envelopeFirstByte else {
            return nil
        }
        // Not a valid push-routing envelope (domain / version / length
        // mismatch) → `nil` so the caller runs the normal chat parse.
        return try? decodePushRoutingSecret(plaintext: bytes)
    }

    // MARK: - Persist + resolve

    /// Persist a group's secret (device-key-encrypted) AND its
    /// `push_gid → (account, group_id)` reverse-index entry. Idempotent.
    /// Returns the derived `push_gid` bytes, or `nil` when the derivation
    /// fails (should not happen for a 32-byte secret).
    @discardableResult
    public func persistSecret(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data
    ) -> Data? {
        guard let pushGid = Self.derivePushGid(secret: secret) else { return nil }
        store.putRouting(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            secret: secret,
            pushGidHex: Self.hex(pushGid)
        )
        return pushGid
    }

    /// Look up the `push_gid` bytes for a group, or `nil` when this device
    /// does not (yet) hold the secret. Used by the notify path (publisher)
    /// and the subscribe path.
    public func pushGid(accountIdB64: String, groupIdHex: String) -> Data? {
        guard let secret = store.getSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            return nil
        }
        return Self.derivePushGid(secret: secret)
    }

    /// Reverse-map a `push_gid` (lowercase hex) to `(account, group_id)`.
    /// Exposed for the Sprint-4 NSE decrypt reverse-map (Sprint 3 only
    /// POPULATES this via ``persistSecret``).
    public func lookupRoute(pushGidHex: String)
        -> (accountIdB64: String, groupIdHex: String)? {
        store.lookupRoute(pushGidHex: pushGidHex)
    }

    // MARK: - Subscribe / receive / redistribute

    /// Subscribe this device to push wakes for a group UNDER ITS
    /// `push_gid`. A member that does not hold the secret **defers** — it
    /// never subscribes under the leaky `group_id`. Best-effort: failures
    /// are logged and swallowed (push is a UX nicety; foreground polling
    /// still works).
    public func subscribeUnderPushGidBestEffort(
        accountIdB64: String,
        groupIdHex: String
    ) async {
        guard let pushGid = pushGid(accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            // No secret held yet → DEFER. Never subscribe under group_id.
            return
        }
        do {
            try await subscribeUnderPushGid(pushGid)
        } catch {
            NSLog("push_gid subscribe failed: \(error)")
        }
    }

    /// RECEIVE side — a member got the distribution envelope: persist the
    /// secret (idempotent) + subscribe under the derived `push_gid`.
    /// Best-effort.
    public func receiveSecret(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data
    ) async {
        guard persistSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret) != nil else {
            return
        }
        await subscribeUnderPushGidBestEffort(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    /// Re-broadcast the CURRENT secret to a group (used right after an
    /// add-member commit so the new member learns it on their next poll).
    /// No-op when this device does not hold the secret. Idempotent for
    /// existing members (a re-received secret is the same value → an
    /// idempotent re-persist). Best-effort.
    public func redistribute(
        accountIdB64: String,
        groupIdHex: String,
        broadcast: (Data) async throws -> Void
    ) async {
        guard let secret = store.getSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            return
        }
        do {
            try await broadcast(try Self.composeSecretEnvelope(secret: secret))
        } catch {
            NSLog("push-routing secret redistribute failed: \(error)")
        }
    }

    // MARK: - Owner-mint funnel

    /// Ensure a group's push routing is provisioned for this device.
    ///
    ///   * If the local client is the OWNER (ADR-0022) and no secret is
    ///     held yet: mint + persist + broadcast the distribution envelope
    ///     (via `broadcast`, which posts an E2EE MLS application message to
    ///     the group journal).
    ///   * Then subscribe under `push_gid` if the secret is held (owner
    ///     just minted it; a member may already hold one from a prior
    ///     receive), else defer.
    ///
    /// This single funnel serves BOTH the chat-open path (owner opens the
    /// freshly founded group) AND the legacy-sweep (owner opens /
    /// rehydrates an un-provisioned group). Non-owners never mint here —
    /// they only subscribe if they already hold a secret; otherwise they
    /// wait for the receive path.
    ///
    /// A per-group in-flight guard coalesces a concurrent legacy-sweep +
    /// onAppear. The mint → persist is synchronous (no `await` before
    /// ``PushRoutingStore/hasSecret(accountIdB64:groupIdHex:)`` flips true),
    /// so even absent the guard a second concurrent call would never
    /// re-mint; the guard additionally suppresses a redundant re-broadcast.
    ///
    /// - Parameter broadcast: posts the distribution-envelope bytes to the
    ///   group journal as an E2EE MLS application message
    ///   (persist-before-publish). Only invoked on the owner-mint branch.
    public func ensureProvisioned(
        accountIdB64: String,
        groupIdHex: String,
        group: Group,
        identity: Identity,
        broadcast: (Data) async throws -> Void
    ) async {
        let lockKey = accountIdB64 + "\u{0}" + groupIdHex
        if inflight.contains(lockKey) { return }
        inflight.insert(lockKey)
        defer { inflight.remove(lockKey) }

        // ADR-0022 role: `.owner`. `nil` = open / legacy group with no role
        // table — nobody mints (avoids divergence), matching the ADR-0042
        // open-question decision to keep un-owned legacy groups on deferred
        // push until a natural re-creation.
        let isOwner = (try? group.ownRole(identity: identity)) == .owner

        if isOwner, !store.hasSecret(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            guard let secret = Self.mintSecret() else { return }
            // Mint → persist synchronously (flips `hasSecret` true) before
            // any await, then distribute to founding members over the E2EE
            // journal. Best-effort beyond the persist — a broadcast failure
            // retries on the next launch / add-member; the local secret +
            // reverse index are already durable.
            if persistSecret(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret) != nil {
                do {
                    try await broadcast(try Self.composeSecretEnvelope(secret: secret))
                } catch {
                    NSLog("push-routing secret broadcast failed: \(error)")
                }
            }
        }

        // Subscribe (or defer) under push_gid. Owner just minted →
        // subscribes; a member with no secret → deferred (never under
        // group_id).
        await subscribeUnderPushGidBestEffort(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    // MARK: - Store passthrough

    /// Wipe every push-routing secret + reverse-index entry for the
    /// account. Called from ``ChatRepository/forgetDevice()``.
    public func clear(accountIdB64: String) {
        store.clear(accountIdB64: accountIdB64)
    }

    // MARK: - Helpers

    /// Lowercase-hex a byte buffer (the push-gw pseudonym wire form).
    public nonisolated static func hex(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}
