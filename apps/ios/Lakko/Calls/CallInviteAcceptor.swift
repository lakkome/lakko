// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Joiner-side acceptor orchestration for link-based call
/// invites (Phase 3c3-acceptor-helper).
///
/// Symmetric sibling of `CallInviteComposer` — collapses the
/// seven-step joiner pipeline into a single discriminated
/// result so the `/c/<slug>` landing screen can render the
/// right outcome copy without re-implementing the dispatch:
///
///   1. ``CallInviteUrl/parseCallInviteSlug(_:)`` — the ledger
///      id (an inline slug, the removed form, is malformed).
///   2. ``DiscoveryClient/fetchCallInvite(blobId:)`` — bytes
///      or 404.
///   3. `CallInviteCapability.decodeAndVerify(wire:)` — local
///      Ed25519 signature check (catches a malicious server
///      serving substituted bytes).
///   4. `capability.isValidNow(nowMs:)` — expiry filter.
///   5. ``DiscoveryClient/consumeCallInvite(blobId:capabilityBytes:joinerAccountId:)``
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
        /// `429` on the ledger fetch or consume: the link may be
        /// fine, the server asked this network address to slow
        /// down. "Try again later", never "invalid".
        case rateLimited
        case malformedSlug(reason: String)
        /// ADR-0028 amendment 2026-09-28 — the fragment secret is missing or
        /// malformed, or the blob does not open under it.
        case damaged
        case signatureFailure(reason: String)
        case failed(message: String)
    }

    // MARK: - Pipeline

    /// End-to-end "join call link" pipeline.
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
        // ---- 1. Parse slug + fragment ------------------------
        // `slug` is the link token `slug#fragment`: the secret the capability
        // is sealed under rides the fragment.
        let parts = InviteLinkSecret.split(slug)
        let parsed: CallInviteUrl.ParsedCallInvite
        do {
            parsed = try CallInviteUrl.parseCallInviteSlug(parts.base)
        } catch {
            return .malformedSlug(reason: (error as? LocalizedError)?.errorDescription
                ?? String(describing: error))
        }

        guard let linkSecret = InviteLinkSecret.read(parts.fragment) else {
            return .damaged
        }

        // ---- 2. Fetch the sealed bytes ----------------------
        let sealed: Data
        let blobId: Data
        switch parsed {
        case .pointer(let id):
            blobId = id
            do {
                guard let fetched = try await discoveryClient.fetchCallInvite(
                    blobId: id
                ) else {
                    return .notFound
                }
                sealed = fetched
            } catch let error as DiscoveryClientError {
                if case let .http(status, _, _) = error, status == 404 {
                    return .notFound
                }
                if case .rateLimited = error {
                    return .rateLimited
                }
                return .failed(message: error.errorDescription ?? String(describing: error))
            } catch {
                return .failed(message: (error as? LocalizedError)?.errorDescription
                    ?? String(describing: error))
            }
        }

        // ---- 3. open (decrypt + verify) ---------------------
        let capability: CallInviteCapability
        do {
            capability = try CallInviteCapability.openLink(sealed: sealed, linkSecret: linkSecret)
        } catch {
            return .damaged
        }
        // ADR-0078 — the link's grant names the owner's mailbox through its
        // binding; the core verifies it against the owner's account key (the
        // one-time KeyPackage's device binding, or the leaf key of an unbound
        // leaf). A link whose grant names a mailbox the owner did not bind is
        // refused like a damaged one: nothing may ever be written under it.
        guard Self.linkGrantVerifies(capability) else {
            return .damaged
        }
        // The joiner tag is computed over the PLAINTEXT capability.
        let capabilityBytes = capability.wire()

        // ---- 4. Expiry filter --------------------------------
        if !capability.isValidNow(nowMs: nowMs) {
            return .expired
        }

        // ---- 5. Consume ---------------------------------------
        do {
            let consumeResult: ConsumeInviteResult
            do {
                consumeResult = try await discoveryClient.consumeCallInvite(
                    blobId: blobId,
                    capabilityBytes: capabilityBytes,
                    joinerAccountId: joinerAccountId
                )
            } catch {
                if DiscoveryClientError.isRateLimited(error) {
                    return .rateLimited
                }
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

    /// ADR-0078 — whether the link's `link` grant names a mailbox the owner
    /// bound, as the core decides it
    /// (`CallInviteCapability.verifiedLinkGrantMailboxPk`). `false` refuses
    /// the link as damaged, on every path that opens a `/c/` capability.
    static func linkGrantVerifies(_ capability: CallInviteCapability) -> Bool {
        do {
            _ = try capability.verifiedLinkGrantMailboxPk()
            return true
        } catch {
            NSLog("ADR-0078: a call link's grant does not verify; refused")
            return false
        }
    }
}
