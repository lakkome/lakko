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
    /// The lookup could not complete (network, a 429, no anonymous token, a
    /// refused identity gate). The username is not shown, and neither is the
    /// "Username not verified" line: it reads as a check still running.
    /// Retried by itself after ``UsernameVerifier/retryDelayMs(failures:cap:)``.
    case failed(atMs: Int64)

    var isVerified: Bool { self == .verified }

    /// A result that says "this username is not theirs" rather than "not
    /// known yet". Drives the quiet "Username not verified" line in contact
    /// detail. A failed check is not one (2026-10-24, docs/ux/profile-names.md
    /// section 6).
    var isRefusal: Bool { self == .mismatch }
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
    /// The longest a transient failure stands before the pair is retried.
    nonisolated static let defaultRetryAfterMs: Int64 = 5 * 60 * 1000

    /// The waits before a failed check is tried again: seconds at first, so a
    /// rate limit or a dropped connection clears quickly. Chosen, not measured;
    /// the same steps as the web and Android clients.
    nonisolated static let retryStepsMs: [Int64] = [15_000, 30_000, 60_000, 120_000, 300_000]

    /// The wait before retry number `failures` (1 for the first), never
    /// longer than `cap`.
    nonisolated static func retryDelayMs(failures: Int, cap: Int64) -> Int64 {
        let index = min(max(failures, 1), retryStepsMs.count) - 1
        return min(retryStepsMs[index], cap)
    }

    private let resolve: (String) async throws -> UsernameResolution
    private let nowMs: () -> Int64
    let retryAfterMs: Int64
    private let defaults: UserDefaults?
    private let keyPrefix: String
    /// Whether a failed check schedules its own retry (off in tests that drive
    /// the clock themselves).
    private let autoRetry: Bool

    private var accountIdB64: String?
    private var verdicts: [String: UsernameVerificationState] = [:]
    private var inFlight: [String: Task<UsernameVerificationState, Never>] = [:]
    /// Failed checks in a row per pair, and when the next try is due.
    private var failures: [String: (count: Int, retryAtMs: Int64)] = [:]

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
        keyPrefix: String = "lakko.profiles.username-verdicts.v1.",
        autoRetry: Bool = true
    ) {
        self.resolve = resolve
        self.nowMs = nowMs
        self.retryAfterMs = retryAfterMs
        self.defaults = defaults
        self.keyPrefix = keyPrefix
        self.autoRetry = autoRetry
    }

    /// Switch to `accountIdB64` and load its persisted verdicts.
    func activate(accountIdB64: String?) {
        guard accountIdB64 != self.accountIdB64 else { return }
        self.accountIdB64 = accountIdB64
        verdicts = [:]
        failures = [:]
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
        case .failed(let atMs)?: return nowMs() >= retryAt(key, failedAtMs: atMs)
        }
    }

    /// When a pair that failed may be tried again.
    private func retryAt(_ key: String, failedAtMs: Int64) -> Int64 {
        failures[key]?.retryAtMs ?? (failedAtMs + Self.retryDelayMs(failures: 1, cap: retryAfterMs))
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
                if nowMs() < retryAt(key, failedAtMs: atMs) { return held }
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
        if case .failed = verdict {
            let count = (failures[key]?.count ?? 0) + 1
            let delay = Self.retryDelayMs(failures: count, cap: retryAfterMs)
            failures[key] = (count, nowMs() + delay)
            scheduleRetry(signatureKeyB64: signatureKeyB64, username: username, delayMs: delay)
        } else {
            failures[key] = nil
        }
        return record(verdict, key: key)
    }

    /// Record that this device already verified the pair (a search showed the
    /// username for this key), asking nothing. A definitive verdict held
    /// stands.
    func noteVerified(signatureKeyB64: String, username: String) {
        let key = Self.cacheKey(signatureKeyB64, username)
        switch verdicts[key] {
        case .verified?, .mismatch?:
            return
        case .failed?, nil:
            failures[key] = nil
            _ = record(.verified, key: key)
        }
    }

    /// Try a failed pair again by itself once its wait is over; the screen
    /// re-renders through ``onChange`` when the verdict changes.
    private func scheduleRetry(signatureKeyB64: String, username: String, delayMs: Int64) {
        guard autoRetry else { return }
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(max(0, delayMs)) * 1_000_000)
            guard let self, !Task.isCancelled else { return }
            await self.verify(signatureKeyB64: signatureKeyB64, username: username)
        }
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
