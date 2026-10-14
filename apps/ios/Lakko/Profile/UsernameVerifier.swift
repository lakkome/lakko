// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What discovery says a username resolves to.
enum UsernameResolution: Equatable {
    /// The username is registered and its published identity bundle carries
    /// this Ed25519 signature key.
    case signatureKey(Data)
    /// Nobody holds this username.
    case notRegistered
}

/// The verification result for one `(signature key, claimed username)` pair.
enum UsernameVerificationState: Equatable, Codable {
    /// Discovery resolves the username to exactly this key. The username may
    /// be shown for this person.
    case verified
    /// Discovery resolves the username to a different key, or to nobody. The
    /// username is never shown for this key.
    case mismatch
    /// The lookup could not complete (network, no anonymous token, a refused
    /// identity gate). Not shown either; retried after
    /// ``UsernameVerifier/retryAfterMs``.
    case failed(atMs: Int64)

    var isVerified: Bool { self == .verified }

    /// A result that says "do not show this username" rather than "not known
    /// yet". Drives the quiet "Username not verified" line in contact detail.
    var isRefusal: Bool {
        switch self {
        case .verified: return false
        case .mismatch, .failed: return true
        }
    }
}

/// ADR-0048 §3 — username verification: `lookup -> account_id ->
/// identityFetch -> signature public key`, and a username is verified for a
/// sender only when that key EQUALS the sender's authenticated key.
///
/// * **Cached per `(key, username)`.** A person who changes their claimed
///   username produces a different cache entry, so the new claim is verified
///   from scratch; the old verdict never carries over.
/// * **Single-flight.** Every surface that renders a name may ask at once; a
///   pair already being resolved joins the in-flight lookup instead of paying
///   a second lookup (and a second anonymous token).
/// * **Definitive verdicts persist**, transient failures do not survive a
///   retry window — a phone that was offline must not permanently lose a
///   contact's `@username`.
/// * **Never from the NotificationServiceExtension.** The NSE does not compile
///   this file; a lookup tells discovery that someone resolved that username
///   (ADR-0048 consequences), which only an explicit foreground rendering may
///   cost.
@MainActor
final class UsernameVerifier {
    /// How long a transient failure stands before the pair may be retried.
    nonisolated static let defaultRetryAfterMs: Int64 = 15 * 60 * 1000

    private let resolve: (String) async throws -> UsernameResolution
    private let nowMs: () -> Int64
    let retryAfterMs: Int64
    private let defaults: UserDefaults?
    private let keyPrefix: String

    private var accountIdB64: String?
    private var verdicts: [String: UsernameVerificationState] = [:]
    private var inFlight: [String: Task<UsernameVerificationState, Never>] = [:]

    /// Called on the main actor whenever a verdict changes, so observers can
    /// re-render names in place.
    var onChange: (() -> Void)?

    /// ADR-0060 section 5 — verify `(signature key b64, username)` against the
    /// username certificate the profile carried: `true` verified, `false` a
    /// refusal, a throw a transient failure (retried after `retryAfterMs`).
    /// When set, it replaces ``resolve``.
    var verifyBinding: ((String, String) async throws -> Bool)?

    init(
        resolve: @escaping (String) async throws -> UsernameResolution,
        nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) },
        retryAfterMs: Int64 = UsernameVerifier.defaultRetryAfterMs,
        defaults: UserDefaults? = .standard,
        keyPrefix: String = "lakko.profiles.username-verdicts.v1."
    ) {
        self.resolve = resolve
        self.nowMs = nowMs
        self.retryAfterMs = retryAfterMs
        self.defaults = defaults
        self.keyPrefix = keyPrefix
    }

    /// Switch to `accountIdB64` and load its persisted verdicts.
    func activate(accountIdB64: String?) {
        guard accountIdB64 != self.accountIdB64 else { return }
        self.accountIdB64 = accountIdB64
        verdicts = [:]
        inFlight.values.forEach { $0.cancel() }
        inFlight = [:]
        guard let accountIdB64,
              let data = defaults?.data(forKey: keyPrefix + accountIdB64),
              let loaded = try? JSONDecoder().decode([String: UsernameVerificationState].self, from: data)
        else { return }
        verdicts = loaded.filter { if case .failed = $0.value { return false } else { return true } }
    }

    /// The verdict held for the pair, or `nil` when it has not been verified.
    func state(signatureKeyB64: String, username: String) -> UsernameVerificationState? {
        verdicts[Self.cacheKey(signatureKeyB64, username)]
    }

    /// `true` when a ``verify(signatureKeyB64:username:)`` call would do real
    /// work: no verdict yet, or a transient failure whose retry window has
    /// passed, and no lookup already in flight.
    func needsVerification(signatureKeyB64: String, username: String) -> Bool {
        let key = Self.cacheKey(signatureKeyB64, username)
        guard inFlight[key] == nil else { return false }
        switch verdicts[key] {
        case nil: return true
        case .verified?, .mismatch?: return false
        case .failed(let atMs)?: return nowMs() - atMs >= retryAfterMs
        }
    }

    /// Verify the pair if it is not already decided. Returns the verdict.
    ///
    /// A pair with a definitive verdict is answered from the cache; a pair
    /// whose transient failure is still inside its retry window is answered
    /// with that failure; a pair already in flight joins that lookup.
    @discardableResult
    func verify(signatureKeyB64: String, username: String) async -> UsernameVerificationState {
        let key = Self.cacheKey(signatureKeyB64, username)
        if let held = verdicts[key] {
            switch held {
            case .verified, .mismatch:
                return held
            case .failed(let atMs):
                if nowMs() - atMs < retryAfterMs { return held }
            }
        }
        if let running = inFlight[key] {
            return await running.value
        }
        guard let expected = Data(base64Encoded: signatureKeyB64), expected.count == 32,
              !username.isEmpty else {
            return record(.mismatch, key: key)
        }
        let resolve = self.resolve
        let nowMs = self.nowMs
        let bindingCheck = self.verifyBinding
        let task = Task<UsernameVerificationState, Never> {
            do {
                // ADR-0060 section 5 — a certificate check, when wired, replaces
                // the lookup: a person hidden from search answers no lookup,
                // and no endpoint confirms a guessed username for a known key.
                if let bindingCheck {
                    return try await bindingCheck(signatureKeyB64, username) ? .verified : .mismatch
                }
                switch try await resolve(username) {
                case .signatureKey(let resolved):
                    return resolved == expected ? .verified : .mismatch
                case .notRegistered:
                    return .mismatch
                }
            } catch {
                return .failed(atMs: nowMs())
            }
        }
        inFlight[key] = task
        let verdict = await task.value
        // Only the flight that is still registered may record: an account
        // switch mid-lookup cancelled and forgot it.
        guard inFlight[key] == task else { return verdict }
        inFlight[key] = nil
        return record(verdict, key: key)
    }

    /// Drop the account's persisted verdicts.
    func clear(accountIdB64: String) {
        defaults?.removeObject(forKey: keyPrefix + accountIdB64)
        if self.accountIdB64 == accountIdB64 {
            verdicts = [:]
        }
    }

    private func record(_ verdict: UsernameVerificationState, key: String) -> UsernameVerificationState {
        let changed = verdicts[key] != verdict
        verdicts[key] = verdict
        // Not written once the account's session ended (``AccountStoreGate``).
        if let accountIdB64, let defaults, let gate = AccountStoreGate.enter(accountIdB64) {
            defer { gate.leave() }
            let durable = verdicts.filter { if case .failed = $0.value { return false } else { return true } }
            if let data = try? JSONEncoder().encode(durable) {
                defaults.set(data, forKey: keyPrefix + accountIdB64)
            }
        }
        if changed { onChange?() }
        return verdict
    }

    /// `key|username`. Base64 never contains `|`, and a canonical username is
    /// `[a-z0-9_]`, so the join is unambiguous.
    private static func cacheKey(_ signatureKeyB64: String, _ username: String) -> String {
        signatureKeyB64 + "|" + username
    }
}
