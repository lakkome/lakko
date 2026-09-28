// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The discovery round-trips the authenticate flows depend on: the username
/// claim, the username lookup, and the identity-bundle publish.
///
/// A seam, not a new client: production is ``DiscoveryAccountDirectory``,
/// which forwards to ``DiscoveryClient``; tests inject a fake so the signup
/// claim, the signup publish and the login resolution can be driven without a
/// server.
///
/// Username normalisation is NOT done here. ``DiscoveryClient`` blinds the
/// username through `OprfBlinder.blind`, which normalises in Rust
/// (`lakko_discovery_client::normalize_username`: trim, strip a leading `@`,
/// NFKC, lowercase) and — since ADR-0048 §4 — refuses a name outside
/// `[a-z0-9_]{3,32}`. The forms check the same rule first through the FFI
/// `validateUsername` (``checkUsername(_:validate:)``), so a non-conforming
/// name is refused with its reason before any network call, and no second
/// rule exists on iOS.
protocol AccountDirectory {
    /// Bind `username` to `accountId` (`POST /discovery/register`). The
    /// endpoint is session-token gated, so the token of the account being
    /// bound is required. Throws ``DiscoveryClientError/http(status:endpoint:body:)``
    /// with status 409 when the name is already bound.
    func registerUsername(
        accountId: Data,
        username: String,
        sessionToken: String
    ) async throws -> RegisterResult

    /// Resolve `username` to its account id (`POST /discovery/lookup`),
    /// `account == nil` when the name is not registered.
    func lookup(username: String) async throws -> LookupResult

    /// Upsert the account's identity bundle (`POST /discovery/identity`,
    /// session-token gated). See
    /// ``DiscoveryClient/uploadIdentityBundle(accountId:signaturePubkey:x25519Pubkey:supersessionProofB64:leafAuth:genesisAttest:)``.
    func publishIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        leafAuth: Data?,
        genesisAttest: Data?,
        sessionToken: String
    ) async throws
}

/// The production directory — a fresh ``DiscoveryClient`` per call, carrying
/// the session token only where the endpoint needs it.
struct DiscoveryAccountDirectory: AccountDirectory {
    func registerUsername(
        accountId: Data,
        username: String,
        sessionToken: String
    ) async throws -> RegisterResult {
        try await DiscoveryClient(sessionToken: sessionToken)
            .registerUsername(accountId: accountId, username: username)
    }

    func lookup(username: String) async throws -> LookupResult {
        try await DiscoveryClient().lookup(username: username)
    }

    func publishIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        leafAuth: Data?,
        genesisAttest: Data?,
        sessionToken: String
    ) async throws {
        try await DiscoveryClient(sessionToken: sessionToken).uploadIdentityBundle(
            accountId: accountId,
            signaturePubkey: signaturePubkey,
            x25519Pubkey: x25519Pubkey,
            leafAuth: leafAuth,
            genesisAttest: genesisAttest
        )
    }
}

// MARK: - Signup: claim the username

/// The username the user asked for at signup is already bound to another
/// account. Surfaced by the signup form as-is.
public struct UsernameTaken: Error, LocalizedError, Equatable {
    /// The canonical (normalised) form that collided.
    public let username: String

    public init(username: String) {
        self.username = username
    }

    public var errorDescription: String? {
        "The username @\(username) is already taken. Pick another."
    }
}

/// Claim the signup username in discovery.
///
/// Mirror of the claim step in `apps/web/src/auth/AuthContext.tsx` `signup`
/// (`discovery.registerUsername` right after the OPAQUE registration). The
/// web decision this mirrors, and why:
///
///   * The claim needs the new account's id AND its session token (the
///     register endpoint is bearer-gated), and both only exist once the
///     OPAQUE registration committed. So the server-side account necessarily
///     exists before the claim can run.
///   * The claim runs BEFORE anything is persisted on the device and before
///     the session lands. A refusal (409 taken, or any other failure) is
///     thrown to the signup form and leaves the device untouched: the user
///     never ends up signed in to an account nobody can find, and picks
///     another name.
///   * The cost, accepted on web and here: the refused attempt leaves an
///     unreachable server-side account behind (no username, no device holding
///     its keys). A retry with a different name registers a new account.
///
/// - Returns: the canonical username the server bound.
/// - Throws: ``UsernameTaken`` on a 409; any other error unchanged.
func claimSignupUsername(
    accountId: Data,
    username: String,
    sessionToken: String,
    directory: AccountDirectory
) async throws -> String {
    do {
        return try await directory
            .registerUsername(accountId: accountId, username: username, sessionToken: sessionToken)
            .normalisedUsername
    } catch DiscoveryClientError.http(let status, _, _) where status == 409 {
        throw UsernameTaken(username: checkUsername(username).canonical ?? normalizeUsername(input: username))
    }
}

// MARK: - Login: resolve the username

/// The account a password login targets.
struct ResolvedLoginAccount: Equatable {
    let accountIdB64: String
    let username: String
    /// `true` when the on-device marker answered and no lookup ran.
    let fromCache: Bool
}

/// Resolve which account a password login targets.
///
/// Mirror of `apps/web/src/auth/AuthContext.tsx` `login`:
///
///   1. The on-device `(accountId, username)` marker answers when the typed
///      username names the same account — the fast path, no round-trip. The
///      comparison is on the canonical form (the FFI `normalizeUsername`), so
///      `@Alice` and `alice` both hit a marker holding `alice`.
///   2. Otherwise — no marker (a fresh device) or a different name — a
///      discovery lookup resolves the name.
///   3. An unregistered name fails with the same `invalid credentials` a
///      wrong password produces, as on web. A distinct message would leak
///      nothing the public lookup endpoint does not already answer, but the
///      login form keeps one failure copy on every client.
///
/// Identity is untouched by this step: it only picks the account id OPAQUE
/// authenticates against. The identity still comes from the cached phrase
/// seed or the recovery-phrase prompt (ADR-0017).
func resolveLoginAccount(
    cached: CachedAccount?,
    typedUsername: String,
    normalize: (String) -> String = { normalizeUsername(input: $0) },
    validate: (String) throws -> String = { try validateUsername(input: $0) },
    lookup: (String) async throws -> LookupResult,
    encodeAccountId: (Data) -> String
) async throws -> ResolvedLoginAccount {
    let canonical = normalize(typedUsername)
    // `@` alone normalises to nothing — and a guest account's marker holds
    // the empty username, so an empty canonical form must never match it.
    guard !canonical.isEmpty else {
        throw AuthClientError.argument("username must be non-empty")
    }
    // ADR-0048 §4 — a name that fails the username rule is refused with its
    // reason before the marker compare and before any lookup. No conforming
    // client can register such a name, so there is nothing to sign in to.
    _ = try requireValidUsername(
        typedUsername,
        emptyError: AuthClientError.argument("username must be non-empty"),
        validate: validate
    )
    if let cached, normalize(cached.username) == canonical {
        return ResolvedLoginAccount(
            accountIdB64: cached.accountIdB64,
            username: cached.username,
            fromCache: true
        )
    }
    let result = try await lookup(typedUsername)
    guard let account = result.account else {
        throw AuthClientError.http(
            status: 401,
            endpoint: "/login/start",
            body: "invalid credentials"
        )
    }
    return ResolvedLoginAccount(
        accountIdB64: encodeAccountId(account),
        username: result.normalisedUsername,
        fromCache: false
    )
}

// MARK: - Sign-in: refresh the published identity

/// What the directory served for this account when a sign-in looked
/// (`GET /discovery/identity/{account}`).
enum PublishedIdentityObservation: Equatable {
    /// The fetch failed (network, non-404 status, undecodable body).
    case unreachable
    /// 404 — nothing published for this account yet.
    case absent
    /// A bundle is published; `signaturePubkey` is its decoded Ed25519 key,
    /// `nil` when the served value did not decode.
    case present(signaturePubkey: Data?)
}

/// What a sign-in does about the account's published identity bundle.
struct PublishedIdentityRefreshPlan: Equatable {
    /// The directory serves a DIFFERENT signature key than this device
    /// derives from its phrase seed (the ADR-0017 ACCESS-hijack signature).
    let raiseAlert: Bool
    /// Attach an auth-attested genesis proof (ADR-0036) to the upload. Only
    /// when the bundle is not known to be published: on a re-publish of a key
    /// already in the log the attestation is ignored, so minting one would be
    /// an auth round-trip for nothing.
    let mintGenesisAttestation: Bool
}

/// Decide the post-sign-in identity-bundle refresh.
///
/// Mirror of `refreshPublishedIdentity` in `apps/web/src/auth/AuthContext.tsx`
/// on an explicit sign-in: observe first, alert on a different key, then
/// re-publish this device's bundle unconditionally (the upload is always
/// made, so it is not part of the plan). The re-publish is what makes an
/// account reachable when its bundle was never published — a contact request
/// and the ADR-0017 fresh-device phrase check both need it — and it heals a
/// replaced bundle.
func planPublishedIdentityRefresh(
    observed: PublishedIdentityObservation,
    derivedSignaturePubkey: Data
) -> PublishedIdentityRefreshPlan {
    switch observed {
    case .unreachable, .absent:
        return PublishedIdentityRefreshPlan(raiseAlert: false, mintGenesisAttestation: true)
    case .present(let signaturePubkey):
        return PublishedIdentityRefreshPlan(
            raiseAlert: signaturePubkey != derivedSignaturePubkey,
            mintGenesisAttestation: false
        )
    }
}

// MARK: - Form copy

extension DiscoveryClientError {
    /// The signup / login form message for a directory failure (the username
    /// claim or the login lookup). A taken name never reaches here — it is
    /// ``UsernameTaken`` — and an unregistered name is `invalid credentials`.
    var authFormMessage: String {
        switch self {
        case .argument(let message):
            return message
        case .http(let status, _, _):
            switch status {
            case 429:
                return "Too many requests. Please wait a moment and retry."
            case 500...599:
                return "Server error. Please try again shortly."
            default:
                return "The username directory refused the request (\(status))."
            }
        case .network:
            return "Network error. Check your connection and try again."
        case .rateLimited:
            return "Too many requests. Please wait a moment and retry."
        }
    }
}
