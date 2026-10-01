// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0061 section 5 (S6) — signed translation updates between releases.
///
/// The ``ReleaseUpdateChecker`` pattern: at launch and on return to the
/// foreground, throttled to once a day, fetch the signed bundle from a
/// static URL (through the ADR-0026 tunnel when armed), have
/// `l10nAcceptBundle` verify it against the pinned publisher key and the
/// version / release rules, keep the raw bytes, and rebuild the live
/// catalog so the UI changes at once.
///
/// Every device downloads the same bytes whatever its language, and the
/// request carries nothing else, so the server cannot tell which language
/// a device uses (UX spec section 7).
///
/// Fails toward the built-in translations, silently: an unpinned build is
/// dormant, and any fetch / verify / rollback / storage failure keeps the
/// current state and only logs. The core also never lets a bundle replace
/// a string the built-in catalog marks security-critical.
@MainActor
final class TranslationUpdater {
    /// The process-wide updater over ``Localization/shared``.
    static let shared = TranslationUpdater(localization: .shared)

    /// Throttle between checks that got an answer (the release-notice
    /// cadence, ADR-0038).
    static let checkThrottleMillis: Int64 = 24 * 60 * 60 * 1000

    /// The non-secret throttle timestamp (plain `UserDefaults`).
    static let lastCheckKey = "lakko.l10n.bundle.lastCheckAtMillis"

    /// Everything outside this type, injectable for tests.
    struct Environment {
        /// The pinned publisher key, `nil` = dormant.
        var verifyingKey: () -> Data?
        /// The bundle URL.
        var bundleUrl: () -> URL?
        /// This build's release number (``ReleasePin/currentRelease``).
        var currentRelease: () -> UInt64
        /// One GET. The live value rides the anti-censorship tunnel when armed.
        var fetch: (URLRequest) async throws -> (Data, URLResponse)
        /// `l10nAcceptBundle`: bytes, key, current release, built-in version,
        /// last accepted.
        var accept: @Sendable (Data, Data, UInt64, UInt64, UInt64?) throws -> L10nBundle

        static let live = Environment(
            verifyingKey: { TranslationPin.verifyingKey },
            bundleUrl: { TranslationPin.bundleUrl },
            currentRelease: { ReleasePin.currentRelease },
            fetch: { request in
                try await AntiCensorshipTransport.perform(
                    request, session: .shared, config: AntiCensorshipConfig.current)
            },
            accept: { bytes, key, release, builtIn, last in
                try l10nAcceptBundle(
                    bundle: bytes,
                    verifyingKey: key,
                    currentRelease: release,
                    builtInVersion: builtIn,
                    lastAccepted: last
                )
            }
        )
    }

    private let localization: Localization
    private let store: TranslationBundleStore
    private let defaults: UserDefaults
    private let env: Environment
    private var restored = false
    private var running = false

    init(
        localization: Localization,
        store: TranslationBundleStore = FileTranslationBundleStore(),
        defaults: UserDefaults = .standard,
        environment: Environment = .live
    ) {
        self.localization = localization
        self.store = store
        self.defaults = defaults
        self.env = environment
    }

    /// Restore the stored bundle (first call only), then fetch if due. Safe to
    /// call at launch and at every return to the foreground; a call while one
    /// is running returns at once.
    func check(now: Date = Date()) async {
        guard !running else { return }
        running = true
        defer { running = false }
        guard let key = env.verifyingKey(), let builtIn = localization.builtInVersion else {
            // Dormant (or no source catalog): nothing can be verified, so a
            // bundle kept by an earlier build is not used either.
            if !restored {
                restored = true
                if store.load() != nil {
                    store.delete()
                    NSLog("[l10n] no translation key pinned, stored update deleted")
                }
            }
            return
        }
        if !restored {
            restored = true
            await restoreStored(key: key, builtIn: builtIn)
        }
        let nowMillis = Int64(now.timeIntervalSince1970 * 1000)
        let last = (defaults.object(forKey: Self.lastCheckKey) as? NSNumber)?.int64Value
        guard translationCheckDue(lastCheckMillis: last, nowMillis: nowMillis, throttleMillis: Self.checkThrottleMillis),
              let url = env.bundleUrl() else {
            return
        }
        await fetchAndApply(url: url, key: key, builtIn: builtIn, nowMillis: nowMillis)
    }

    // MARK: - Internals

    private func restoreStored(key: Data, builtIn: UInt64) async {
        guard let stored = store.load() else { return }
        let accept = env.accept
        let release = env.currentRelease()
        let bytes = stored.bytes
        // Off the main actor: the payload carries every language.
        let bundle = await Task.detached(priority: .utility) {
            try? accept(bytes, key, release, builtIn, nil)
        }.value
        switch storedTranslationVerdict(storedVersion: stored.version, reverifiedVersion: bundle?.version()) {
        case .use:
            localization.applyTranslationBundle(bundle)
            NSLog("[l10n] stored translation update \(stored.version) in use")
        case .discard:
            store.delete()
            NSLog("[l10n] stored translation update \(stored.version) no longer verifies, deleted")
        }
    }

    private func fetchAndApply(url: URL, key: Data, builtIn: UInt64, nowMillis: Int64) async {
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        let data: Data
        do {
            let (body, response) = try await env.fetch(request)
            let status = (response as? HTTPURLResponse)?.statusCode
            if translationResponseCountsAsCheck(httpStatus: status) {
                defaults.set(NSNumber(value: nowMillis), forKey: Self.lastCheckKey)
            } else {
                NSLog("[l10n] translation update fetch HTTP \(status.map(String.init) ?? "none"), keeping current")
                return
            }
            data = body
        } catch {
            NSLog("[l10n] translation update fetch failed, keeping current: \(error)")
            return
        }
        let lastAccepted = localization.translationBundle?.version() ?? store.load()?.version
        let accept = env.accept
        let release = env.currentRelease()
        let result: Result<L10nBundle, Error> = await Task.detached(priority: .utility) {
            Result { try accept(data, key, release, builtIn, lastAccepted) }
        }.value
        let bundle: L10nBundle
        switch result {
        case .success(let accepted):
            bundle = accepted
        case .failure(let error):
            NSLog("[l10n] translation update not accepted, keeping current: \(error)")
            return
        }
        do {
            try store.save(StoredTranslationBundle(
                bytes: data, version: bundle.version(), signedAtMillis: bundle.signedAtMillis()))
        } catch {
            // Still used for this run; the next launch fetches it again.
            NSLog("[l10n] translation update not stored: \(error)")
        }
        localization.applyTranslationBundle(bundle)
        NSLog("[l10n] translation update \(bundle.version()) applied")
    }
}
