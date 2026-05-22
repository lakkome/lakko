// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Joiner-side acceptor orchestration for link-based call
/// invites (Phase 3c3-acceptor-helper).
///
/// Symmetric sibling of `CallInviteComposer` — collapses the
/// seven-step joiner pipeline into a single discriminated
/// result so the `/c/<slug>` landing screen can render the
/// right outcome copy without re-implementing the dispatch:
///
///   1. ``CallInviteUrl/parseCallInviteSlug(_:)`` — pointer
///      vs. inline.
///   2. (pointer branch)
///      ``DiscoveryClient/fetchCallInvite(blobId:)`` — bytes
///      or 404.
///   3. `CallInviteCapability.decodeAndVerify(wire:)` — local
///      Ed25519 signature check (catches a malicious server
///      serving substituted bytes).
///   4. `capability.isValidNow(nowMs:)` — expiry filter.
///   5. (pointer branch)
///      ``DiscoveryClient/consumeCallInvite(blobId:capabilityBytes:joinerAccountId:)``
///      — atomic counter bump + per-joiner dedup tag.
///   6. `acceptCallInvite(capability:joinerIdentity:)` —
///      bootstrap a fresh MLS group with joiner as founder +
///      owner added via the embedded KP.
///   7. Return `welcomeForOwner` bytes for the platform to
///      ferry back to the owner.
///
/// Sibling of `apps/web/src/calls/acceptor.ts` +
/// `apps/android/.../calls/CallInviteAcceptor.kt` — same
/// pipeline shape, platform-idiomatic surface.
public enum CallInviteAcceptor {

    // MARK: - Discriminated result

    /// Discriminated outcome. Every variant carries a non-
    /// failure payload OR a stable error tag — the landing
    /// screen branches exhaustively without throwing.
    public enum Result: Equatable {
        case accepted(
            capabilityBytes: Data,
            ownerAccountId: Data,
            callId: Data,
            ownerFingerprint: String,
            welcomeForOwner: Data,
            /// 32-byte SFrame call-key seed derived via the
            /// MLS exporter (`mls_exporter("lakko/call/v1",
            /// call_id, 32)`). UI hands it to
            /// `sframeDeriveParticipantKey` to seal the
            /// joiner's first frame before the owner has
            /// even joined.
            callKeySeed: Data
        )
        /// `(blob_id, joiner_tag)` PK already had a row —
        /// counter NOT bumped. UI surfaces "you're already
        /// in this call" + opens the existing call group
        /// rather than re-bootstrapping.
        case alreadyJoined(capabilityBytes: Data)
        case expired
        case exhausted
        case notFound
        case malformedSlug(reason: String)
        case signatureFailure(reason: String)
        case failed(message: String)
    }

    // MARK: - Pipeline

    /// End-to-end "join call link" pipeline.
    ///
    /// Inline-capability slugs (censored-region fallback)
    /// skip the `fetch` + `consume` steps entirely — consume
    /// on an inline capability has no server-side row to
    /// bump. The acceptor surfaces this asymmetry as
    /// `.accepted(...)` with `capabilityBytes` populated; the
    /// `welcomeForOwner` still needs ferrying via whatever
    /// transport the user has.
    ///
    /// - Parameters:
    ///   - slug:            The `<slug>` segment from a
    ///                      `/c/<slug>` URL.
    ///   - joinerAccountId: Joiner's 16-byte UUIDv7
    ///                      account_id — folded into the
    ///                      per-joiner dedup tag locally
    ///                      before posting to consume.
    ///                      Never crosses the wire in
    ///                      plaintext.
    ///   - joinerIdentity:  Joiner's MLS identity handle —
    ///                      drives the
    ///                      `acceptCallInvite(...)` call that
    ///                      bootstraps the fresh call MLS
    ///                      group.
    ///   - discoveryClient: Discovery client bound to the
    ///                      joiner's session token (or
    ///                      anonymous).
    ///   - nowMs:           UNIX millis the acceptor uses
    ///                      for the
    ///                      `capability.isValidNow(...)`
    ///                      check. Defaults to "now".
    public static func acceptFromSlug(
        slug: String,
        joinerAccountId: Data,
        joinerIdentity: Identity,
        discoveryClient: DiscoveryClient,
        nowMs: UInt64 = UInt64(Date().timeIntervalSince1970 * 1000)
    ) async -> Result {
        // ---- 1. Parse slug -----------------------------------
        let parsed: CallInviteUrl.ParsedCallInvite
        do {
            parsed = try CallInviteUrl.parseCallInviteSlug(slug)
        } catch {
            return .malformedSlug(reason: (error as? LocalizedError)?.errorDescription
                ?? String(describing: error))
        }

        // ---- 2. Pointer branch: fetch bytes -----------------
        let capabilityBytes: Data
        var blobId: Data? = nil
        switch parsed {
        case .pointer(let id):
            blobId = id
            do {
                guard let fetched = try await discoveryClient.fetchCallInvite(
                    blobId: id
                ) else {
                    return .notFound
                }
                capabilityBytes = fetched
            } catch let error as DiscoveryClientError {
                if case let .http(status, _, _) = error, status == 404 {
                    return .notFound
                }
                return .failed(message: error.errorDescription ?? String(describing: error))
            } catch {
                return .failed(message: (error as? LocalizedError)?.errorDescription
                    ?? String(describing: error))
            }
        case .inline(let bytes):
            capabilityBytes = bytes
        }

        // ---- 3. decodeAndVerify -----------------------------
        let capability: CallInviteCapability
        do {
            capability = try CallInviteCapability.decodeAndVerify(wire: capabilityBytes)
        } catch {
            return .signatureFailure(reason: (error as? LocalizedError)?.errorDescription
                ?? String(describing: error))
        }

        // ---- 4. Expiry filter --------------------------------
        if !capability.isValidNow(nowMs: nowMs) {
            return .expired
        }

        // ---- 5. Pointer branch: consume ----------------------
        if let blobId = blobId {
            let consumeResult: ConsumeInviteResult
            do {
                consumeResult = try await discoveryClient.consumeCallInvite(
                    blobId: blobId,
                    capabilityBytes: capabilityBytes,
                    joinerAccountId: joinerAccountId
                )
            } catch {
                return .failed(message: (error as? LocalizedError)?.errorDescription
                    ?? String(describing: error))
            }
            switch consumeResult {
            case .notFound:
                return .notFound
            case .exhausted:
                return .exhausted
            case .alreadyConsumed(_, _):
                return .alreadyJoined(capabilityBytes: capabilityBytes)
            case .consumed(_, _):
                break // fall through to accept
            }
        }

        // ---- 6. acceptCallInvite (MLS bootstrap) -------------
        let accepted: AcceptedCallInvite
        do {
            accepted = try acceptCallInvite(
                capability: capability,
                joinerIdentity: joinerIdentity
            )
        } catch {
            return .failed(message: "accept_call_invite: " + ((error as? LocalizedError)?.errorDescription
                ?? String(describing: error)))
        }

        // ---- 7. Done -----------------------------------------
        return .accepted(
            capabilityBytes: capabilityBytes,
            ownerAccountId: capability.ownerAccountId(),
            callId: capability.callId(),
            ownerFingerprint: capability.ownerFingerprint(),
            welcomeForOwner: accepted.welcomeForOwner(),
            callKeySeed: accepted.callKeySeed()
        )
    }
}
