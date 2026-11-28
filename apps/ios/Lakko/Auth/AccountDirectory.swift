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

    /// `POST /discovery/username/replace` (ADR-0076): retire the account's
    /// current name and make `username` its name.
    func replaceUsername(
        accountId: Data, username: String, sessionToken: String
    ) async throws -> ReplacedUsername

    /// Upsert the account's identity bundle (`POST /discovery/identity`,
    /// session-token gated). `keyResetAttest` is the ADR-0073 key-reset
    /// attestation on the upload that installs reset keys, `nil` otherwise.
    /// See
    /// ``DiscoveryClient/uploadIdentityBundle(accountId:signaturePubkey:x25519Pubkey:x25519Binding:supersessionProofB64:leafAuth:genesisAttest:keyResetAttest:)``.
    func publishIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        x25519Binding: Data,
        leafAuth: Data?,
        genesisAttest: Data?,
        keyResetAttest: Data?,
        sessionToken: String
    ) async throws

    /// ADR-0074 section 3.5 — the proven-rotation upload: the new identity
    /// bundle with the supersession proof (signed by the OLD key, which
    /// discovery verifies against the key its row holds) AND the key-reset
    /// attestation. `supersessionProof` is `nil` for the fallback (the row no
    /// longer holds the key the proof was signed by); `keyResetAttest` is
    /// `nil` only when the rotation's answer was lost. `conversationLink` is
    /// the ADR-0074 section 3.7 link naming the key the conversations are
    /// bound to, on the fallback too: the upload changes the key, and a key
    /// change that carries no link clears the stored one.
    func publishRotatedIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        x25519Binding: Data,
        supersessionProof: Data?,
        keyResetAttest: Data?,
        conversationLink: Data?,
        sessionToken: String
    ) async throws
}

extension AccountDirectory {
    /// ADR-0101 section 10 — the attested publish of a key reset with its
    /// reset seal (`nil` for an unsealed reset). A directory that knows no
    /// seals (a test fake) publishes the bundle without one.
    func publishResetIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        x25519Binding: Data,
        leafAuth: Data?,
        keyResetAttest: Data?,
        resetSeal: Data?,
        sessionToken: String
    ) async throws {
        try await publishIdentityBundle(
            accountId: accountId, signaturePubkey: signaturePubkey, x25519Pubkey: x25519Pubkey,
            x25519Binding: x25519Binding, leafAuth: leafAuth, genesisAttest: nil,
            keyResetAttest: keyResetAttest, sessionToken: sessionToken)
    }

    /// ADR-0101 section 10 — publish the account's seal announcement: the
    /// same identity bundle again, with the announcement. A directory that
    /// knows no seals (a test fake) refuses.
    func publishSealAnnouncement(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        x25519Binding: Data,
        announcement: Data,
        sessionToken: String
    ) async throws {
        throw DiscoveryClientError.argument("this directory cannot publish a seal announcement")
    }

    /// A directory that predates the rotation (a test fake) refuses it.
    func publishRotatedIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        x25519Binding: Data,
        supersessionProof: Data?,
        keyResetAttest: Data?,
        conversationLink: Data?,
        sessionToken: String
    ) async throws {
        throw DiscoveryClientError.argument("this directory cannot publish a rotated identity")
    }
}

/// The production directory — a fresh ``DiscoveryClient`` per call, carrying
/// the session token only where the endpoint needs it.
struct DiscoveryAccountDirectory: AccountDirectory {
    func replaceUsername(
        accountId: Data, username: String, sessionToken: String
    ) async throws -> ReplacedUsername {
        let replaced = try await DiscoveryClient(sessionToken: sessionToken)
            .replaceUsername(accountId: accountId, username: username)
        return ReplacedUsername(
            normalisedUsername: replaced.normalisedUsername, certificate: replaced.certificate)
    }

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
        x25519Binding: Data,
        leafAuth: Data?,
        genesisAttest: Data?,
        keyResetAttest: Data?,
        sessionToken: String
    ) async throws {
        try await DiscoveryClient(sessionToken: sessionToken).uploadIdentityBundle(
            accountId: accountId,
            signaturePubkey: signaturePubkey,
            x25519Pubkey: x25519Pubkey,
            x25519Binding: x25519Binding,
            leafAuth: leafAuth,
            genesisAttest: genesisAttest,
            keyResetAttest: keyResetAttest
        )
    }

    func publishResetIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        x25519Binding: Data,
        leafAuth: Data?,
        keyResetAttest: Data?,
        resetSeal: Data?,
        sessionToken: String
    ) async throws {
        try await DiscoveryClient(sessionToken: sessionToken).uploadIdentityBundle(
            accountId: accountId,
            signaturePubkey: signaturePubkey,
            x25519Pubkey: x25519Pubkey,
            x25519Binding: x25519Binding,
            leafAuth: leafAuth,
            keyResetAttest: keyResetAttest,
            resetSeal: resetSeal
        )
    }

    func publishSealAnnouncement(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        x25519Binding: Data,
        announcement: Data,
        sessionToken: String
    ) async throws {
        try await DiscoveryClient(sessionToken: sessionToken).uploadIdentityBundle(
            accountId: accountId,
            signaturePubkey: signaturePubkey,
            x25519Pubkey: x25519Pubkey,
            x25519Binding: x25519Binding,
            resetSeals: announcement
        )
    }

    func publishRotatedIdentityBundle(
        accountId: Data,
        signaturePubkey: Data,
        x25519Pubkey: Data,
        x25519Binding: Data,
        supersessionProof: Data?,
        keyResetAttest: Data?,
        conversationLink: Data?,
        sessionToken: String
    ) async throws {
        // No `leaf_auth`: discovery stores and forwards the proof as the log
        // entry's authorisation ("T5 as built" item 19).
        try await DiscoveryClient(sessionToken: sessionToken).uploadIdentityBundle(
            accountId: accountId,
            signaturePubkey: signaturePubkey,
            x25519Pubkey: x25519Pubkey,
            x25519Binding: x25519Binding,
            supersessionProofB64: supersessionProof.map { base64Encode($0) },
            leafAuth: nil,
            genesisAttest: nil,
            keyResetAttest: keyResetAttest,
            conversationLink: conversationLink
        )
    }
}

/// What a rename's discovery step answers.
struct ReplacedUsername: Equatable {
    let normalisedUsername: String
    /// The new name's certificate, when the server sent one.
    let certificate: Data?
}

extension AccountDirectory {
    /// `POST /discovery/username/replace` (ADR-0076). A directory that does
    /// not implement it refuses: a test double scripts its own.
    func replaceUsername(
        accountId: Data, username: String, sessionToken: String
    ) async throws -> ReplacedUsername {
        throw DiscoveryClientError.argument("username replace is not wired")
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

/// `429 {"error":"handle-bind-limited"}` from `PUT /auth/login-handle`: the
/// account changed its username too often (`docs/ux/anti-abuse.md` section 2).
/// The signup claim stops before discovery, as for a taken name, and the form
/// keeps its contents.
public struct UsernameBindLimited: Error, LocalizedError, Equatable {
    public let retryAfterSecs: Int?

    public init(retryAfterSecs: Int?) {
        self.retryAfterSecs = retryAfterSecs
    }

    public var errorDescription: String? {
        AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: retryAfterSecs)
    }
}

/// Binding a login handle at auth (ADR-0054 S5). ``AuthClient`` in
/// production; a fake in tests.
protocol LoginHandleBinder {
    /// `PUT /auth/login-handle`. Throws `AuthClientError.http(409, ...)` when
    /// another account holds the handle.
    func bindLoginHandle(handle: Data, sessionToken: String) async throws
    /// The same bind carrying a held supporter place (ADR-0097) in
    /// `x-lakko-anon-token`; `nil` is the plain bind.
    func bindLoginHandle(handle: Data, sessionToken: String, supporterToken: String?) async throws
    /// `DELETE /auth/login-handle`.
    @discardableResult
    func removeLoginHandle(sessionToken: String) async throws -> Bool
}

extension LoginHandleBinder {
    func bindLoginHandle(handle: Data, sessionToken: String, supporterToken: String?) async throws {
        try await bindLoginHandle(handle: handle, sessionToken: sessionToken)
    }
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
///      the name is taken: ``UsernameTaken``, and discovery is never asked. A
///      `429 handle-bind-limited` stops here too: ``UsernameBindLimited``.
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
/// - Throws: ``UsernameTaken``; ``RegistrationClosed`` when the beta is full
///   (`403 registration-closed` at the bind); ``UsernameRuleViolation`` for a
///   name that fails the rule; any other error unchanged.
func claimSignupUsername(
    accountId: Data,
    username: String,
    sessionToken: String,
    directory: AccountDirectory,
    binder: LoginHandleBinder,
    supporterToken: String? = nil,
    derive: (String) throws -> Data = { try loginHandle(username: $0) }
) async throws -> String {
    let name = try LoginName.make(
        username,
        emptyError: AuthClientError.argument("username must be non-empty"),
        derive: derive
    )
    do {
        try await binder.bindLoginHandle(
            handle: name.handle, sessionToken: sessionToken, supporterToken: supporterToken)
    } catch AuthClientError.http(let status, _, _) where status == 409 {
        throw UsernameTaken(username: name.canonical)
    } catch let error as AuthClientError {
        // `docs/ux/beta-account-cap.md` — the beta is full: a first username
        // is refused at auth, and discovery is never asked.
        if SignupCap.isRegistrationClosed(error) {
            NSLog("[auth] registration closed")
            throw RegistrationClosed()
        }
        // ADR-0097 — the held supporter place was spent by another account.
        if SignupCap.isSupporterCodeUsed(error) {
            NSLog("[auth] supporter code already used")
            throw SupporterCodeUsed()
        }
        // ADR-0054 S5 + anti-abuse section 2 — too many handle changes: stop
        // before discovery like a taken name, with its own copy.
        if let refusal = AuthRateLimit.refusal(in: error),
           refusal.code == AuthRateLimit.handleBindLimited {
            throw UsernameBindLimited(retryAfterSecs: refusal.retryAfterSecs)
        }
        throw error
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

// MARK: - Sign-in: the published identity

// The decision a session landing takes about the account's published
// identity is ``KeyResetLanding`` (ADR-0073 section 4.2): land, re-publish,
// or refuse to upload because this device's keys are not the account's any
// more. The observation it consumes is read in
// ``AuthRepository/landWithPublishedIdentityCheck``.

// MARK: - Form copy

extension DiscoveryClientError {
    /// The signup form message for a directory failure (the username claim or
    /// the identity publish). A taken name never reaches here: it is
    /// ``UsernameTaken``.
    var authFormMessage: String {
        AuthErrorCopy.shared(self) ?? Localization.text("auth.signup.error.generic")
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
    static var otherAccountCopy: String { Localization.text("device_link.error.name_other_account") }
    /// The lookup could not run; the generic network failure copy.
    static var unavailableCopy: String { Localization.text("common.error.offline") }

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
