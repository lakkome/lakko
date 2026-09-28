// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The discovery round-trips the authenticate flows depend on: the username
/// claim and the identity-bundle publish.
///
/// A seam, not a new client: production is ``DiscoveryAccountDirectory``,
/// which forwards to ``DiscoveryClient``; tests inject a fake so the signup
/// claim and the signup publish can be driven without a server.
///
/// There is deliberately no username lookup here (ADR-0054 S5): sign-in and
/// recovery name the account by its login handle at auth (``LoginName``),
/// and nothing before a session may ask discovery who a name is.
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

/// Binding a login handle at auth (ADR-0054 S5). ``AuthClient`` in
/// production; a fake in tests.
protocol LoginHandleBinder {
    /// `PUT /auth/login-handle`. Throws `AuthClientError.http(409, ...)` when
    /// another account holds the handle.
    func bindLoginHandle(handle: Data, sessionToken: String) async throws
    /// `DELETE /auth/login-handle`.
    @discardableResult
    func removeLoginHandle(sessionToken: String) async throws -> Bool
}

extension AuthClient: LoginHandleBinder {}

/// Claim a username: the login handle at auth FIRST, then the name at
/// discovery (ADR-0054 S5, "Claim order").
///
/// The handle and the discovery name live in two first-come registries that
/// cannot check each other (neither server knows the name). Claiming in one
/// order on every honest client leaves a squatter only names nobody claimed
/// anywhere yet, which is ordinary first-come naming:
///
///   1. `PUT /auth/login-handle` with the new account's session. A `409` means
///      the name is taken: ``UsernameTaken``, and discovery is never asked.
///   2. `POST /discovery/register`. A `409` there is also ``UsernameTaken``.
///      ANY failure of this step releases the handle again
///      (`DELETE /auth/login-handle`, best-effort): the signup form keeps the
///      device untouched and the account behind it is abandoned, and a handle
///      left bound to it would answer `409` to the user's own retry.
///
/// Mirror of the claim step in `apps/web/src/auth/AuthContext.tsx` `signup`.
/// The claim needs the new account's id and session token, both of which
/// exist only once the OPAQUE registration committed, so a refused claim
/// leaves an unreachable server-side account behind (accepted on every
/// client). A guest account (ADR-0032) has no username and never calls this.
///
/// - Returns: the canonical username the server bound.
/// - Throws: ``UsernameTaken``; ``UsernameRuleViolation`` for a name that
///   fails the rule; any other error unchanged.
func claimSignupUsername(
    accountId: Data,
    username: String,
    sessionToken: String,
    directory: AccountDirectory,
    binder: LoginHandleBinder,
    derive: (String) throws -> Data = { try loginHandle(username: $0) }
) async throws -> String {
    let name = try LoginName.make(
        username,
        emptyError: AuthClientError.argument("username must be non-empty"),
        derive: derive
    )
    do {
        try await binder.bindLoginHandle(handle: name.handle, sessionToken: sessionToken)
    } catch AuthClientError.http(let status, _, _) where status == 409 {
        throw UsernameTaken(username: name.canonical)
    }
    do {
        return try await directory
            .registerUsername(accountId: accountId, username: name.canonical, sessionToken: sessionToken)
            .normalisedUsername
    } catch {
        do {
            try await binder.removeLoginHandle(sessionToken: sessionToken)
        } catch let releaseError {
            NSLog("[auth] login handle release after a failed claim failed: \(releaseError)")
        }
        if case DiscoveryClientError.http(let status, _, _) = error, status == 409 {
            throw UsernameTaken(username: name.canonical)
        }
        throw error
    }
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
    /// The signup form message for a directory failure (the username claim or
    /// the identity publish). A taken name never reaches here: it is
    /// ``UsernameTaken``.
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

// MARK: - Device restore: the discovery reads (ADR-0056 security pass)

/// The two discovery reads a no-password device restore makes before it
/// persists anything: the published identity the transferred seed must
/// reproduce, and the account the typed username resolves to. Injectable so
/// the restore's ordering is testable without a network.
protocol DeviceRestoreDirectory {
    /// `GET /discovery/identity/{account}`; `nil` on 404.
    func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse?

    /// The ordinary username lookup (OPRF, one `discovery-lookup` token drawn
    /// with `sessionToken`, then the anonymous `POST /discovery/lookup`):
    /// the 16-byte account id `username` resolves to, `nil` when discovery
    /// does not know the name. `accountId` is the account `sessionToken`
    /// belongs to (whose allowance the token is drawn from); it never
    /// travels on the lookup itself.
    func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data?
}

/// The production reads, over short-lived clients.
struct DiscoveryDeviceRestoreDirectory: DeviceRestoreDirectory {
    func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? {
        try await DiscoveryClient(sessionToken: sessionToken).identityFetch(accountId: accountId)
    }

    func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? {
        // The session has not landed: the token is drawn with it directly,
        // never through the shared pool (which would store tokens for an
        // account the restore may still refuse). The lookup itself stays
        // anonymous (invariant #5): the client carries no session on it.
        let tokens = DetachedAnonTokenSource(issuer: AuthAnonTokenIssuer(), sessionToken: sessionToken)
        return try await DiscoveryClient(tokens: tokens).lookup(username: username).account
    }
}

/// Why a no-password device restore refused the name check.
public enum TypedNameCheckRefusal: Error, LocalizedError, Equatable {
    /// Discovery resolves the typed name to another account, or to none.
    case otherAccount
    /// The lookup could not run (network, token quota, tokens switched off).
    case unavailable

    public var errorDescription: String? {
        switch self {
        case .otherAccount: return TypedNameCheck.otherAccountCopy
        case .unavailable: return TypedNameCheck.unavailableCopy
        }
    }
}

/// ADR-0056 security pass — the typed name must resolve, at discovery, to the
/// account the grant redeem answered for.
///
/// The fresh device's request is routed by the auth login handle of the typed
/// name, and a squatted handle can route it to another account whose device
/// confirms; the fresh device would then land in that account under the typed
/// name. Discovery is a registry auth does not control (its own service,
/// database and OPRF key), so it is asked too. Fail closed: a mismatch, an
/// unknown name and a lookup that could not run all refuse. Mirror of
/// `apps/web/src/auth/device-restore-name-check.ts`.
enum TypedNameCheck {
    /// The name resolves to another account, or to none.
    static let otherAccountCopy =
        "The name you typed belongs to a different account. Nothing was changed on this device."
    /// The lookup could not run; the generic network failure copy.
    static let unavailableCopy = "Could not reach Lakko. Check your connection and try again."

    /// - Throws: ``TypedNameCheckRefusal``.
    static func check(
        username: String,
        accountIdBytes: Data,
        lookup: (String) async throws -> Data?
    ) async throws {
        let resolved: Data?
        do {
            resolved = try await lookup(username)
        } catch {
            NSLog("device restore: typed-name lookup failed; refusing (%@)", String(describing: type(of: error)))
            throw TypedNameCheckRefusal.unavailable
        }
        guard let resolved, constantTimeEquals(resolved, accountIdBytes) else {
            NSLog(
                resolved == nil
                    ? "device restore: typed name unknown to discovery; refusing"
                    : "device restore: typed name resolves to another account; refusing"
            )
            throw TypedNameCheckRefusal.otherAccount
        }
    }

    private static func constantTimeEquals(_ a: Data, _ b: Data) -> Bool {
        guard a.count == b.count else { return false }
        var diff: UInt8 = 0
        for (x, y) in zip(a, b) { diff |= x ^ y }
        return diff == 0
    }
}
